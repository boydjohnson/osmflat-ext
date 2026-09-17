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
//! contiguous, power-of-two span of child indices. Within a bucket a pair
//! packs into one `u64` key -- the child's offset in the span above the
//! parent's bits -- so sorting keys sorts pairs. Then the buckets are read
//! back in child order, each sorted on its own, and streamed straight into the
//! output CSR
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
//! RAM. Bucket appends are sequential, and the output is written in order.
//! The node->ways pairs are collected in parallel over way ranges, and buckets
//! are sorted in parallel, one window of as many buckets as Rayon has threads
//! at a time, so at most that many buckets are in RAM at once. With
//! `--mmap-scratch` the buckets are temp files in that directory; without it
//! they stay in RAM.

use crate::scratch::KeySink;
use crate::{BuildError, BuildOptions};
use osmflat::{Osm, RelationMembersRef};
use osmflat_ext::{BackrefsBuilder, Range, Ref};
use rayon::prelude::*;
use std::ops::Range as IndexRange;
use std::path::Path;
use std::sync::Mutex;
use std::time::Instant;

/// Pairs per bucket to aim for: a bucket is sorted in RAM, so this bounds each
/// sort's working set (8 bytes a pair: ~128 MB). Up to one bucket per Rayon
/// thread is held at once.
const TARGET_PAIRS_PER_BUCKET: usize = 16 << 20;

/// Pairs a collect worker buffers per bucket before appending them to the
/// shared bucket sink under its lock, in one write.
const LOCAL_BUFFER_PAIRS: usize = 16 << 10;

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
    let mut phase_start = Instant::now();
    let node_ways = PairBuckets::new(scratch, n_nodes, n_ways, parent.nodes_index().len())?;
    collect_way_nodes(parent, &node_ways)?;
    eprintln!("[backrefs] node->ways collect: {:?}", phase_start.elapsed());
    phase_start = Instant::now();
    node_ways.write_csr(
        builder.start_node_ways_range()?,
        builder.start_ways_of_node()?,
    )?;
    eprintln!(
        "[backrefs] node->ways sort + write: {:?}",
        phase_start.elapsed()
    );

    // node/way/relation -> relations: the three maps share one pass. Each
    // member yields at most one pair, in exactly one of the maps.
    phase_start = Instant::now();
    let n_members = parent.relation_members().len();
    let mut rels_of_node = PairBuckets::new(scratch, n_nodes, n_rels, n_members)?;
    let mut rels_of_way = PairBuckets::new(scratch, n_ways, n_rels, n_members)?;
    let mut rels_of_rel = PairBuckets::new(scratch, n_rels, n_rels, n_members)?;
    for_each_member(parent, |member, r| match member {
        Member::Node(n) => rels_of_node.push(n, r),
        Member::Way(w) => rels_of_way.push(w, r),
        Member::Relation(rr) => rels_of_rel.push(rr, r),
    })?;
    eprintln!(
        "[backrefs] X->relations collect: {:?}",
        phase_start.elapsed()
    );
    phase_start = Instant::now();
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
    eprintln!(
        "[backrefs] X->relations sort + write: {:?}",
        phase_start.elapsed()
    );
    Ok(())
}

/// A relation member resolved to its parent-archive index.
enum Member {
    Node(u64),
    Way(u64),
    Relation(u64),
}

/// Push each distinct (node, way) pair into `buckets`. Ways are split into
/// contiguous index ranges collected in parallel; pairs reach a bucket in no
/// particular order, which is fine since each bucket is sorted before it is
/// written.
fn collect_way_nodes(parent: &Osm, buckets: &PairBuckets) -> Result<(), BuildError> {
    let nodes_index = parent.nodes_index();
    let ways = parent.ways();
    // Several ranges per thread, so a dense range doesn't leave threads idle.
    let n_ranges = (rayon::current_num_threads() * 8).min(ways.len().max(1));
    let ranges: Vec<IndexRange<usize>> = (0..n_ranges)
        .map(|i| i * ways.len() / n_ranges..(i + 1) * ways.len() / n_ranges)
        .collect();
    ranges.into_par_iter().try_for_each(|range| {
        let mut local = LocalPairs::new(buckets);
        let mut nodes = Vec::new();
        for w in range {
            nodes.clear();
            nodes.extend(
                ways[w]
                    .refs()
                    .filter_map(|ri| nodes_index[ri as usize].value()),
            );
            nodes.sort_unstable();
            nodes.dedup();
            for &n in &nodes {
                local.push(n, w as u64)?;
            }
        }
        local.flush()
    })
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

/// One reverse map's `(child, parent)` pairs, bucketed by child index so that
/// concatenating the sorted buckets in order yields every pair sorted by
/// `(child, parent)`.
///
/// Bucket `b` holds children `b << span_bits .. (b + 1) << span_bits`, and a
/// pair is stored as the key `(child - bucket start) << parent_bits | parent`.
/// `span_bits + parent_bits <= 64`, so the key is exact, and within a bucket
/// key order is `(child, parent)` order.
struct PairBuckets {
    buckets: Vec<Mutex<KeySink>>,
    n_children: u64,
    span_bits: u32,
    parent_bits: u32,
}

impl PairBuckets {
    /// Buckets for up to `max_pairs` pairs over children `0..n_children` and
    /// parents `0..n_parents`.
    fn new(
        dir: Option<&Path>,
        n_children: usize,
        n_parents: usize,
        max_pairs: usize,
    ) -> Result<Self, BuildError> {
        let parent_bits = u64::BITS - (n_parents.saturating_sub(1) as u64).leading_zeros();
        // Aim for `TARGET_PAIRS_PER_BUCKET`, rounding the span up to a power of
        // two, but never so wide that a child offset overflows its bits.
        let want_buckets = max_pairs.div_ceil(TARGET_PAIRS_PER_BUCKET).max(1);
        let want_span = n_children.div_ceil(want_buckets).max(1) as u64;
        let span_bits = (u64::BITS - (want_span - 1).leading_zeros())
            .min(u64::BITS - parent_bits)
            .min(u64::BITS - 1);
        let n_buckets = ((n_children as u64).div_ceil(1 << span_bits)).max(1);
        let buckets = (0..n_buckets)
            .map(|_| KeySink::new(dir).map(Mutex::new))
            .collect::<Result<_, _>>()?;
        Ok(PairBuckets {
            buckets,
            n_children: n_children as u64,
            span_bits,
            parent_bits,
        })
    }

    /// The bucket holding `child`, and the pair's key within it.
    fn locate(&self, child: u64, parent: u64) -> (usize, u64) {
        let offset = child & ((1u64 << self.span_bits) - 1);
        let key = (offset << self.parent_bits) | parent;
        ((child >> self.span_bits) as usize, key)
    }

    /// Recover the pair from bucket `bucket`'s `key`.
    fn decode(&self, bucket: usize, key: u64) -> (u64, u64) {
        let offset = key >> self.parent_bits;
        let parent = key & ((1u64 << self.parent_bits) - 1);
        (((bucket as u64) << self.span_bits) | offset, parent)
    }

    /// Push one pair from a single-threaded collect pass.
    fn push(&mut self, child: u64, parent: u64) -> Result<(), BuildError> {
        let (bucket, key) = self.locate(child, parent);
        self.buckets[bucket]
            .get_mut()
            .expect("bucket lock poisoned")
            .extend(&[key])
    }

    /// Stream the map out as a CSR pair: one `Range` per child (plus the
    /// trailing sentinel flatdata trims on read) whose `post()` slices the
    /// flattened parents.
    fn write_csr(
        mut self,
        mut ranges: flatdata::ExternalVector<Range>,
        mut posts: flatdata::ExternalVector<Ref>,
    ) -> Result<(), BuildError> {
        let mut next_child = 0u64;
        let mut written = 0u64;
        let window = rayon::current_num_threads();
        let n_buckets = self.buckets.len();
        let mut buckets = std::mem::take(&mut self.buckets)
            .into_iter()
            .map(|b| b.into_inner().expect("bucket lock poisoned"));
        let mut bucket_idx = 0;
        while bucket_idx < n_buckets {
            // Read back and sort the next window of buckets in parallel, then
            // write them out in bucket order. `par_sort_unstable` also
            // parallelizes a lone bucket, the common case for the small
            // relation maps.
            let sorted: Vec<Vec<u64>> = buckets
                .by_ref()
                .take(window)
                .collect::<Vec<_>>()
                .into_par_iter()
                .map(|bucket| {
                    let mut keys = bucket.into_keys()?;
                    keys.par_sort_unstable();
                    Ok(keys)
                })
                .collect::<Result<_, BuildError>>()?;
            for keys in sorted {
                for key in keys {
                    let (child, parent) = self.decode(bucket_idx, key);
                    // Open the ranges of every child up to this one; children
                    // with no parents get empty ranges.
                    while next_child <= child {
                        ranges.grow()?.set_first_idx(written);
                        next_child += 1;
                    }
                    posts.grow()?.set_value(parent);
                    written += 1;
                }
                bucket_idx += 1;
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

/// A collect worker's per-bucket buffers over shared [`PairBuckets`]: keys are
/// batched locally and appended under a bucket's lock only when its buffer
/// fills, so workers rarely contend.
struct LocalPairs<'a> {
    buckets: &'a PairBuckets,
    buffers: Vec<Vec<u64>>,
}

impl<'a> LocalPairs<'a> {
    fn new(buckets: &'a PairBuckets) -> Self {
        LocalPairs {
            buckets,
            buffers: vec![Vec::new(); buckets.buckets.len()],
        }
    }

    fn push(&mut self, child: u64, parent: u64) -> Result<(), BuildError> {
        let (bucket, key) = self.buckets.locate(child, parent);
        let buffer = &mut self.buffers[bucket];
        buffer.push(key);
        if buffer.len() >= LOCAL_BUFFER_PAIRS {
            Self::append(&self.buckets.buckets[bucket], buffer)?;
        }
        Ok(())
    }

    /// Append every buffered key. Must be called when the worker finishes.
    fn flush(mut self) -> Result<(), BuildError> {
        for (bucket, buffer) in self.buckets.buckets.iter().zip(&mut self.buffers) {
            Self::append(bucket, buffer)?;
        }
        Ok(())
    }

    fn append(bucket: &Mutex<KeySink>, buffer: &mut Vec<u64>) -> Result<(), BuildError> {
        if !buffer.is_empty() {
            bucket
                .lock()
                .expect("bucket lock poisoned")
                .extend(buffer)?;
            buffer.clear();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::PairBuckets;

    /// Keys round-trip to their pairs, land in the bucket covering the child,
    /// and sort in `(child, parent)` order within a bucket, across edge cases
    /// for the bit split.
    #[test]
    fn pair_keys_round_trip_and_sort() {
        for (n_children, n_parents, max_pairs) in [
            (1, 1, 1),
            (0, 0, 0),
            (10, 1, 10),
            (1000, 2, 5000),
            (1 << 20, 1 << 16, 1 << 24),
            ((1 << 20) + 1, (1 << 16) + 1, 1 << 26),
            (1 << 40, 1 << 30, 1 << 36),
        ] {
            let buckets = PairBuckets::new(None, n_children, n_parents, max_pairs).unwrap();
            assert!(buckets.span_bits + buckets.parent_bits <= u64::BITS);
            let n_buckets = buckets.buckets.len() as u64;
            assert!(n_buckets << buckets.span_bits >= n_children as u64);

            let last_child = (n_children as u64).saturating_sub(1);
            let last_parent = (n_parents as u64).saturating_sub(1);
            let mut pairs = vec![(0, 0), (0, last_parent), (last_child, 0)];
            pairs.push((last_child, last_parent));
            pairs.push((last_child / 2, last_parent / 2));
            pairs.push((last_child / 2, last_parent / 2 + last_parent % 2));
            for &(child, parent) in &pairs {
                let (bucket, key) = buckets.locate(child, parent);
                assert!((bucket as u64) < n_buckets);
                assert_eq!(buckets.decode(bucket, key), (child, parent));
            }

            let mut located: Vec<_> = pairs.iter().map(|&(c, p)| buckets.locate(c, p)).collect();
            located.sort_unstable();
            let decoded: Vec<_> = located.iter().map(|&(b, k)| buckets.decode(b, k)).collect();
            let mut expected = pairs.clone();
            expected.sort_unstable();
            assert_eq!(decoded, expected);
        }
    }
}
