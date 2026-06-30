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
    postings: &'a [Ref],
    ranges: &'a [Range<u64>],
) -> impl Iterator<Item = u64> + 'a {
    ranges.iter().flat_map(move |r| {
        let lo = postings.partition_point(|p| p.value() < r.start);
        let hi = postings.partition_point(|p| p.value() < r.end);
        postings[lo..hi].iter().map(|p| p.value())
    })
}

/// Intersect two ascending postings slices (e.g. `k1=v1 ∩ k2=v2`).
/// Linear merge, `O(k1 + k2)`.
pub fn intersect<'a>(a: &'a [Ref], b: &'a [Ref]) -> impl Iterator<Item = u64> + 'a {
    let (mut i, mut j) = (0usize, 0usize);
    std::iter::from_fn(move || {
        while i < a.len() && j < b.len() {
            let (av, bv) = (a[i].value(), b[j].value());
            if av == bv {
                i += 1;
                j += 1;
                return Some(av);
            } else if av < bv {
                i += 1;
            } else {
                j += 1;
            }
        }
        None
    })
}

/// Union of several ascending postings slices (e.g. `key=*` over a key's value
/// postings), ascending and deduplicated.
///
/// Phase 1 collects-sorts-dedups for correctness; the streaming upgrade is a
/// loser-tree / `BinaryHeap` k-way merge (design §4, `key=*`).
pub fn union(lists: &[&[Ref]]) -> impl Iterator<Item = u64> {
    let mut out: Vec<u64> = lists
        .iter()
        .flat_map(|l| l.iter().map(|p| p.value()))
        .collect();
    out.sort_unstable();
    out.dedup();
    out.into_iter()
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
        // Intersect smallest term first to keep intermediates small.
        let mut terms = self.terms;
        terms.sort_by_key(|t| t.len());
        let mut acc: Vec<u64> = match terms.first() {
            Some(first) => first.iter().map(|p| p.value()).collect(),
            None => return Vec::new(),
        };
        for term in &terms[1..] {
            acc = intersect_sorted(&acc, term.iter().map(|p| p.value()));
        }
        if let Some(ranges) = self.bbox_ranges {
            acc = ranges
                .iter()
                .flat_map(|r| {
                    let lo = acc.partition_point(|&v| v < r.start);
                    let hi = acc.partition_point(|&v| v < r.end);
                    acc[lo..hi].iter().copied()
                })
                .collect();
        }
        acc
    }
}

/// Intersect an ascending `acc` with an ascending iterator `b`.
fn intersect_sorted(acc: &[u64], b: impl Iterator<Item = u64>) -> Vec<u64> {
    let mut out = Vec::new();
    let mut i = 0usize;
    for bv in b {
        while i < acc.len() && acc[i] < bv {
            i += 1;
        }
        if i < acc.len() && acc[i] == bv {
            out.push(bv);
            i += 1;
        }
    }
    out
}
