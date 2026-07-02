//! Build the `Backrefs` sub-archive: node->ways and X->relations reverse refs.
//!
//! Two reverse maps (design §5, §6.3):
//!
//! * **node -> ways** from `ways[*].refs() -> nodes_index`: each resolvable way
//!   ref contributes a (node, way) backref.
//! * **X -> relations** from `relation_members`: each `NodeMember` /
//!   `WayMember` / `RelationMember` contributes a (member, relation) backref.
//!
//! Each map is a CSR built count-then-fill (structurally §6.2 phases 1–2,
//! keyed on the parent index): one pass over the parents counts distinct
//! children into offsets, a second fills the postings. Iterating parents
//! (ways, then relations) **in index order** makes each postings run ascending
//! == spatial (SFC) order, so backref results compose with bbox and tag
//! selections. A parent that references the same child twice (a closed way's
//! shared endpoint, a member listed twice) yields one backref: children are
//! collected per parent and sort+dedup'd before counting/filling.
//!
//! Offsets, cursors, and postings all come from [`Scratch`], so
//! `--mmap-scratch` backs the build with temp files at planet scale; without
//! it everything stays in RAM (fine for regional extents).

use crate::scratch::{Scratch, ScratchU64};
use crate::{BuildError, BuildOptions};
use osmflat::{Osm, RelationMembersRef};
use osmflat_ext::{BackrefsBuilder, Range, Ref};

/// Build and write the `Backrefs` sub-archive for `parent` into `builder`.
pub fn build(
    parent: &Osm,
    builder: &BackrefsBuilder,
    opts: &BuildOptions,
) -> Result<(), BuildError> {
    let scratch = Scratch::new(opts.mmap_scratch.as_deref());
    let n_nodes = parent.nodes().len();
    let n_ways = parent.ways().len();
    let n_rels = parent.relations().len();

    // node -> ways: count, then fill.
    let mut node_ways = CsrCounts::new(&scratch, n_nodes)?;
    for_each_way_node(parent, |n, _w| node_ways.count(n));
    let mut node_ways = node_ways.seal(&scratch)?;
    for_each_way_node(parent, |n, w| node_ways.fill(n, w));

    // node/way/relation -> relations: the three maps share the two passes.
    let mut rels_of_node = CsrCounts::new(&scratch, n_nodes)?;
    let mut rels_of_way = CsrCounts::new(&scratch, n_ways)?;
    let mut rels_of_rel = CsrCounts::new(&scratch, n_rels)?;
    for_each_member(parent, |member, _r| match member {
        Member::Node(n) => rels_of_node.count(n),
        Member::Way(w) => rels_of_way.count(w),
        Member::Relation(rr) => rels_of_rel.count(rr),
    });
    let mut rels_of_node = rels_of_node.seal(&scratch)?;
    let mut rels_of_way = rels_of_way.seal(&scratch)?;
    let mut rels_of_rel = rels_of_rel.seal(&scratch)?;
    for_each_member(parent, |member, r| match member {
        Member::Node(n) => rels_of_node.fill(n, r),
        Member::Way(w) => rels_of_way.fill(w, r),
        Member::Relation(rr) => rels_of_rel.fill(rr, r),
    });

    write_csr(
        builder.start_node_ways_range()?,
        builder.start_ways_of_node()?,
        &node_ways,
    )?;
    write_csr(
        builder.start_node_rels_range()?,
        builder.start_rels_of_node()?,
        &rels_of_node,
    )?;
    write_csr(
        builder.start_way_rels_range()?,
        builder.start_rels_of_way()?,
        &rels_of_way,
    )?;
    write_csr(
        builder.start_rel_rels_range()?,
        builder.start_rels_of_rel()?,
        &rels_of_rel,
    )?;
    Ok(())
}

/// A relation member resolved to its parent-archive index.
enum Member {
    Node(u64),
    Way(u64),
    Relation(u64),
}

/// Visit each distinct (node, way) pair, ways in index order — so every node's
/// visits arrive in ascending way order.
fn for_each_way_node(parent: &Osm, mut visit: impl FnMut(u64, u64)) {
    let nodes_index = parent.nodes_index();
    let mut nodes = Vec::new();
    for (w, way) in parent.ways().iter().enumerate() {
        nodes.clear();
        nodes.extend(way.refs().filter_map(|ri| nodes_index[ri as usize].value()));
        nodes.sort_unstable();
        nodes.dedup();
        for &n in &nodes {
            visit(n, w as u64);
        }
    }
}

/// Visit each distinct (member, relation) pair, relations in index order — so
/// every member's visits arrive in ascending relation order.
fn for_each_member(parent: &Osm, mut visit: impl FnMut(Member, u64)) {
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
            visit(Member::Node(n), r as u64);
        }
        for &w in &ways {
            visit(Member::Way(w), r as u64);
        }
        for &rr in &rels {
            visit(Member::Relation(rr), r as u64);
        }
    }
}

/// Count pass of one reverse CSR map: `offsets[child + 1]` accumulates the
/// number of distinct parents referencing `child`.
struct CsrCounts {
    offsets: ScratchU64,
}

impl CsrCounts {
    fn new(scratch: &Scratch, n_children: usize) -> Result<Self, BuildError> {
        Ok(CsrCounts {
            offsets: scratch.alloc(n_children + 1)?,
        })
    }

    fn count(&mut self, child: u64) {
        self.offsets[child as usize + 1] += 1;
    }

    /// Prefix-sum the counts into CSR offsets and allocate the postings array.
    fn seal(mut self, scratch: &Scratch) -> Result<CsrFill, BuildError> {
        for i in 1..self.offsets.len() {
            self.offsets[i] += self.offsets[i - 1];
        }
        let n_children = self.offsets.len() - 1;
        let posts = scratch.alloc(self.offsets[n_children] as usize)?;
        let cursors = scratch.alloc_copy(&self.offsets[..n_children])?;
        Ok(CsrFill {
            offsets: self.offsets,
            cursors,
            posts,
        })
    }
}

/// Fill pass and final CSR of one reverse map: child `c`'s parents are
/// `posts[offsets[c]..offsets[c + 1]]`, ascending.
struct CsrFill {
    offsets: ScratchU64,
    cursors: ScratchU64,
    posts: ScratchU64,
}

impl CsrFill {
    fn fill(&mut self, child: u64, parent: u64) {
        let c = child as usize;
        self.posts[self.cursors[c] as usize] = parent;
        self.cursors[c] += 1;
    }
}

/// Write a CSR pair: one `Range` per parent entity (plus a trailing sentinel
/// flatdata trims on read) whose `post()` slices the flattened `posts` vector.
fn write_csr(
    mut ranges: flatdata::ExternalVector<Range>,
    mut posts: flatdata::ExternalVector<Ref>,
    csr: &CsrFill,
) -> Result<(), BuildError> {
    // `offsets` already carries the sentinel as its last element.
    for &first_idx in csr.offsets.iter() {
        ranges.grow()?.set_first_idx(first_idx);
    }
    for &v in csr.posts.iter() {
        posts.grow()?.set_value(v);
    }
    ranges.close()?;
    posts.close()?;
    Ok(())
}
