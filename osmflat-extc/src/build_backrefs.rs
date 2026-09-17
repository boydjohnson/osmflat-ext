//! Build the `Backrefs` sub-archive: node->ways and X->relations reverse refs.
//!
//! Two reverse maps (design §5, §6.3):
//!
//! * **node -> ways** from `ways[*].refs() -> nodes_index`: each resolvable way
//!   ref contributes a (node, way) backref.
//! * **X -> relations** from `relation_members`: each `NodeMember` /
//!   `WayMember` / `RelationMember` contributes a (member, relation) backref.
//!
//! Each map is built by sorting its `(child, parent)` pairs. One pass over the
//! parents appends every pair to one of several buckets, each covering a
//! contiguous range of child indices; then the buckets are read back in child
//! order, each sorted on its own, and streamed straight into the output CSR
//! (one `Range` per child, then its parents). Sorting by `(child, parent)`
//! makes each child's postings run ascending == spatial (SFC) order, so
//! backref results compose with bbox and tag selections. A parent that
//! references the same child twice (a closed way's shared endpoint, a member
//! listed twice) yields one backref: children are collected per parent and
//! sort+dedup'd before being emitted.
//!
//! This replaces a count-then-fill CSR, which needed several child-sized
//! offset and cursor arrays (5+ GB each for a continent's nodes) written at
//! scattered positions -- page-fault thrashing once they no longer fit in
//! RAM. Bucket appends are sequential, only one bucket is ever held in RAM
//! while sorting, and the output is written in order. With `--mmap-scratch`
//! the buckets are temp files in that directory; without it they stay in RAM.

use crate::scratch::PairSink;
use crate::{BuildError, BuildOptions};
use osmflat::{Osm, RelationMembersRef};
use osmflat_ext::{BackrefsBuilder, Range, Ref};
use std::path::Path;

/// Pairs per bucket to aim for: a bucket is sorted in RAM, so this bounds the
/// sort's working set (16 bytes a pair: ~256 MB).
const TARGET_PAIRS_PER_BUCKET: usize = 16 << 20;

/// Build and write the `Backrefs` sub-archive for `parent` into `builder`.
pub fn build(
    parent: &Osm,
    builder: &BackrefsBuilder,
    opts: &BuildOptions,
) -> Result<(), BuildError> {
    let scratch = opts.mmap_scratch.as_deref();
    let n_nodes = parent.nodes().len();
    let n_ways = parent.ways().len();
    let n_rels = parent.relations().len();

    // node -> ways. Each way ref yields at most one pair.
    let mut node_ways = PairBuckets::new(scratch, n_nodes, parent.nodes_index().len())?;
    for_each_way_node(parent, |n, w| node_ways.push(n, w))?;
    node_ways.write_csr(
        builder.start_node_ways_range()?,
        builder.start_ways_of_node()?,
    )?;

    // node/way/relation -> relations: the three maps share one pass. Each
    // member yields at most one pair, in exactly one of the maps.
    let n_members = parent.relation_members().len();
    let mut rels_of_node = PairBuckets::new(scratch, n_nodes, n_members)?;
    let mut rels_of_way = PairBuckets::new(scratch, n_ways, n_members)?;
    let mut rels_of_rel = PairBuckets::new(scratch, n_rels, n_members)?;
    for_each_member(parent, |member, r| match member {
        Member::Node(n) => rels_of_node.push(n, r),
        Member::Way(w) => rels_of_way.push(w, r),
        Member::Relation(rr) => rels_of_rel.push(rr, r),
    })?;
    rels_of_node.write_csr(
        builder.start_node_rels_range()?,
        builder.start_rels_of_node()?,
    )?;
    rels_of_way.write_csr(
        builder.start_way_rels_range()?,
        builder.start_rels_of_way()?,
    )?;
    rels_of_rel.write_csr(
        builder.start_rel_rels_range()?,
        builder.start_rels_of_rel()?,
    )?;
    Ok(())
}

/// A relation member resolved to its parent-archive index.
enum Member {
    Node(u64),
    Way(u64),
    Relation(u64),
}

/// Visit each distinct (node, way) pair, ways in index order.
fn for_each_way_node(
    parent: &Osm,
    mut visit: impl FnMut(u64, u64) -> Result<(), BuildError>,
) -> Result<(), BuildError> {
    let nodes_index = parent.nodes_index();
    let mut nodes = Vec::new();
    for (w, way) in parent.ways().iter().enumerate() {
        nodes.clear();
        nodes.extend(way.refs().filter_map(|ri| nodes_index[ri as usize].value()));
        nodes.sort_unstable();
        nodes.dedup();
        for &n in &nodes {
            visit(n, w as u64)?;
        }
    }
    Ok(())
}

/// Visit each distinct (member, relation) pair, relations in index order.
fn for_each_member(
    parent: &Osm,
    mut visit: impl FnMut(Member, u64) -> Result<(), BuildError>,
) -> Result<(), BuildError> {
    let members = parent.relation_members();
    let (mut nodes, mut ways, mut rels) = (Vec::new(), Vec::new(), Vec::new());
    for r in 0..parent.relations().len() {
        nodes.clear();
        ways.clear();
        rels.clear();
        for member in members.at(r) {
            match member {
                RelationMembersRef::NodeMember(m) => nodes.extend(m.node_idx()),
                RelationMembersRef::WayMember(m) => ways.extend(m.way_idx()),
                RelationMembersRef::RelationMember(m) => rels.extend(m.relation_idx()),
            }
        }
        for buf in [&mut nodes, &mut ways, &mut rels] {
            buf.sort_unstable();
            buf.dedup();
        }
        for &n in &nodes {
            visit(Member::Node(n), r as u64)?;
        }
        for &w in &ways {
            visit(Member::Way(w), r as u64)?;
        }
        for &rr in &rels {
            visit(Member::Relation(rr), r as u64)?;
        }
    }
    Ok(())
}

/// One reverse map's `(child, parent)` pairs, bucketed by contiguous child
/// index range so that concatenating the sorted buckets in order yields every
/// pair sorted by `(child, parent)`.
struct PairBuckets {
    buckets: Vec<PairSink>,
    n_children: u64,
}

impl PairBuckets {
    /// Buckets for up to `max_pairs` pairs over children `0..n_children`.
    fn new(dir: Option<&Path>, n_children: usize, max_pairs: usize) -> Result<Self, BuildError> {
        let n_buckets = max_pairs
            .div_ceil(TARGET_PAIRS_PER_BUCKET)
            .clamp(1, n_children.max(1));
        let buckets = (0..n_buckets)
            .map(|_| PairSink::new(dir))
            .collect::<Result<_, _>>()?;
        Ok(PairBuckets {
            buckets,
            n_children: n_children as u64,
        })
    }

    fn push(&mut self, child: u64, parent: u64) -> Result<(), BuildError> {
        // Children are spread evenly across buckets by index; `u128` keeps
        // `child * n_buckets` from overflowing.
        let bucket =
            (child as u128 * self.buckets.len() as u128 / self.n_children as u128) as usize;
        self.buckets[bucket].push(child, parent)
    }

    /// Stream the map out as a CSR pair: one `Range` per child (plus the
    /// trailing sentinel flatdata trims on read) whose `post()` slices the
    /// flattened parents.
    fn write_csr(
        self,
        mut ranges: flatdata::ExternalVector<Range>,
        mut posts: flatdata::ExternalVector<Ref>,
    ) -> Result<(), BuildError> {
        let mut next_child = 0u64;
        let mut written = 0u64;
        for bucket in self.buckets {
            let mut pairs = bucket.into_records()?;
            pairs.sort_unstable();
            for (child, parent) in pairs {
                // Open the ranges of every child up to this one; children
                // with no parents get empty ranges.
                while next_child <= child {
                    ranges.grow()?.set_first_idx(written);
                    next_child += 1;
                }
                posts.grow()?.set_value(parent);
                written += 1;
            }
        }
        // The remaining children, then the sentinel at index `n_children`.
        while next_child <= self.n_children {
            ranges.grow()?.set_first_idx(written);
            next_child += 1;
        }
        ranges.close()?;
        posts.close()?;
        Ok(())
    }
}
