//! Boolean composition of postings — the merge-join engine.
//!
//! Every operand here is an **ascending run of parent indices**: a `key=value`
//! postings list (from [`crate::taginfo`]) or the set of entity-index ranges a
//! bbox query returns (from `osmflat::find_*_by_bounding_box`, which yields
//! contiguous spatial-curve ranges). Because both are ascending, intersection
//! and union are linear merges — no temporary hash sets, no re-sort.
//!
//! `tag ∩ bbox` is the headline case: for each of the `R` bbox ranges, binary
//! search that subrange inside the postings list — `O(R·log k)`.

use crate::Ref;
use std::ops::Range;

/// Intersect an ascending postings slice with the ascending, disjoint
/// entity-index `ranges` from a bbox query. Yields parent indices in order.
///
/// `O(R·log k)`: binary-search each range's bounds within `postings`.
pub fn intersect_bbox<'a>(
    _postings: &'a [Ref],
    _ranges: &'a [Range<u64>],
) -> impl Iterator<Item = u64> + 'a {
    todo!("for each range, partition_point the postings bounds, emit the in-range slice");
    #[allow(unreachable_code)]
    std::iter::empty()
}

/// Intersect two ascending postings slices (e.g. `k1=v1 ∩ k2=v2`).
/// Linear merge, `O(k1 + k2)`.
pub fn intersect<'a>(_a: &'a [Ref], _b: &'a [Ref]) -> impl Iterator<Item = u64> + 'a {
    todo!("two-cursor merge over ascending Ref slices, emit equal values");
    #[allow(unreachable_code)]
    std::iter::empty()
}

/// Union of several ascending postings slices (e.g. `key=*` over a key's value
/// postings). k-way merge with dedup.
pub fn union<'a>(_lists: &'a [&'a [Ref]]) -> impl Iterator<Item = u64> + 'a {
    todo!("k-way merge (loser tree / BinaryHeap) over ascending slices, dedup equal");
    #[allow(unreachable_code)]
    std::iter::empty()
}

/// A composable selection of entities of one type: a chain of tag postings to
/// intersect, optionally clipped to a bounding box.
///
/// All terms reduce to ascending-run merges; `into_iter` resolves them.
#[derive(Default)]
pub struct Selection<'a> {
    /// Each term is one ascending postings slice that must contain the entity.
    pub terms: Vec<&'a [Ref]>,
    /// Optional bbox clip, as the disjoint ascending entity-index ranges a
    /// spatial query produced.
    pub bbox_ranges: Option<Vec<Range<u64>>>,
}

impl<'a> Selection<'a> {
    /// Empty selection (matches nothing until a term is added).
    pub fn new() -> Self {
        Self::default()
    }

    /// Require membership in `postings` (logical AND).
    pub fn and(mut self, postings: &'a [Ref]) -> Self {
        self.terms.push(postings);
        self
    }

    /// Clip to the bbox ranges from a spatial query.
    pub fn in_bbox(mut self, ranges: Vec<Range<u64>>) -> Self {
        self.bbox_ranges = Some(ranges);
        self
    }

    /// Resolve to the matching parent indices, ascending.
    pub fn resolve(self) -> Vec<u64> {
        todo!(
            "intersect terms smallest-first, then clip by bbox_ranges; \
             both via the merge helpers above"
        )
    }
}
