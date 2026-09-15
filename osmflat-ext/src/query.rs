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
use osmflat::Osm;
use std::ops::Range;

/// A bounding box in degrees (`lon` = x, `lat` = y), matching the units of
/// `osmflat::find_*_by_bounding_box`.
#[derive(Clone, Copy, Debug)]
pub struct Bbox {
    pub min_lon: f64,
    pub min_lat: f64,
    pub max_lon: f64,
    pub max_lat: f64,
}

/// Index of `item` within the contiguous slice `slice` it points into.
///
/// `osmflat::find_*_by_bounding_box` yields `&Node`/`&Way`/`&Relation`
/// references into the archive's entity slice but not their indices; this
/// recovers the index by offset. Sound because the references point into
/// `slice` and the structs are `repr(transparent)` with a fixed size.
#[inline]
pub(crate) fn slice_index<T>(slice: &[T], item: &T) -> u64 {
    let offset = (item as *const T as usize) - (slice.as_ptr() as usize);
    (offset / std::mem::size_of::<T>()) as u64
}

/// Collect ascending, distinct parent indices, recovering each from a slice.
fn sorted_indices<'a, T: 'a>(base: &[T], items: impl Iterator<Item = &'a T>) -> Vec<u64> {
    let mut v: Vec<u64> = items.map(|it| slice_index(base, it)).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Ascending node indices whose nodes fall in `bbox`, via osmflat's exact
/// spatial query. Drift-free: the candidate ranges and exact-overlap filter are
/// osmflat's own.
pub fn node_indices_in_bbox(archive: &Osm, bbox: Bbox) -> Vec<u64> {
    let base = archive.nodes();
    sorted_indices(
        base,
        osmflat::find_nodes_by_bounding_box(
            archive,
            bbox.min_lon,
            bbox.min_lat,
            bbox.max_lon,
            bbox.max_lat,
        ),
    )
}

/// Ascending way indices overlapping `bbox`. See [`node_indices_in_bbox`].
pub fn way_indices_in_bbox(archive: &Osm, bbox: Bbox) -> Vec<u64> {
    let base = archive.ways();
    sorted_indices(
        base,
        osmflat::find_ways_by_bounding_box(
            archive,
            bbox.min_lon,
            bbox.min_lat,
            bbox.max_lon,
            bbox.max_lat,
        ),
    )
}

/// Ascending relation indices overlapping `bbox`. See [`node_indices_in_bbox`].
pub fn relation_indices_in_bbox(archive: &Osm, bbox: Bbox) -> Vec<u64> {
    let base = archive.relations();
    sorted_indices(
        base,
        osmflat::find_relations_by_bounding_box(
            archive,
            bbox.min_lon,
            bbox.min_lat,
            bbox.max_lon,
            bbox.max_lat,
        ),
    )
}

/// Run-length compress ascending, distinct indices into maximal `[start, end)`
/// runs — the contiguous entity-index ranges that [`intersect_bbox`] consumes.
/// A bbox result is mostly spatially contiguous, so this yields few ranges.
pub fn to_index_ranges(sorted: &[u64]) -> Vec<Range<u64>> {
    let mut out: Vec<Range<u64>> = Vec::new();
    for &v in sorted {
        match out.last_mut() {
            Some(last) if last.end == v => last.end = v + 1,
            _ => out.push(v..v + 1),
        }
    }
    out
}

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
/// Streaming `BinaryHeap` k-way merge (design §4, `key=*`): `O(N log k)` time
/// for `N` total postings across `k` lists, `O(k)` memory, and lazy — taking
/// the first `m` results costs `O(m log k)`, not a full materialize-and-sort.
pub fn union<'a>(lists: &[&'a [Ref]]) -> impl Iterator<Item = u64> + 'a {
    use std::cmp::Reverse;
    use std::collections::BinaryHeap;

    // One cursor per list; the heap holds each cursor's next value, keyed
    // `(value, list)` so equal values pop deterministically.
    let mut cursors: Vec<std::slice::Iter<'a, Ref>> = lists.iter().map(|l| l.iter()).collect();
    let mut heap: BinaryHeap<Reverse<(u64, usize)>> = cursors
        .iter_mut()
        .enumerate()
        .filter_map(|(k, c)| c.next().map(|p| Reverse((p.value(), k))))
        .collect();

    let mut last: Option<u64> = None;
    std::iter::from_fn(move || {
        while let Some(Reverse((v, k))) = heap.pop() {
            if let Some(p) = cursors[k].next() {
                heap.push(Reverse((p.value(), k)));
            }
            if last != Some(v) {
                last = Some(v);
                return Some(v);
            }
        }
        None
    })
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
            acc = clip_to_ranges(&acc, &ranges);
        }
        acc
    }
}

/// Keep the members of ascending `acc` that fall in the ascending, disjoint
/// `ranges`. `O(R·log n)`.
fn clip_to_ranges(acc: &[u64], ranges: &[Range<u64>]) -> Vec<u64> {
    ranges
        .iter()
        .flat_map(|r| {
            let lo = acc.partition_point(|&v| v < r.start);
            let hi = acc.partition_point(|&v| v < r.end);
            acc[lo..hi].iter().copied()
        })
        .collect()
}

/// The entity type a [`Query`] resolves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntityType {
    Node,
    Way,
    Relation,
}

/// Why a [`Query`] could not be resolved.
#[derive(Clone, Debug, PartialEq)]
pub enum QueryError {
    /// The query has no tag and no spatial constraint; resolving it would
    /// return every entity of the type.
    Unconstrained,
    /// A tag constraint was given but the sidecar has no `Taginfo` sub-archive
    /// (build it with `osmflat-extc --taginfo`).
    NoTaginfo,
    /// `within_radius` / `in_polygon` currently only apply to nodes.
    NodesOnly {
        constraint: &'static str,
        entity: EntityType,
    },
    /// A spatial constraint has non-finite coordinates, a negative radius, an
    /// inverted bbox, or a polygon with fewer than 3 vertices.
    InvalidSpatial(&'static str),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unconstrained => write!(f, "query has no tag or spatial constraint"),
            Self::NoTaginfo => write!(
                f,
                "tag constraint needs a Taginfo sub-archive; rebuild with osmflat-extc --taginfo"
            ),
            Self::NodesOnly { constraint, entity } => {
                write!(f, "{constraint} only applies to nodes, not {entity:?}")
            }
            Self::InvalidSpatial(why) => write!(f, "invalid spatial constraint: {why}"),
        }
    }
}

impl std::error::Error for QueryError {}

#[derive(Clone, Debug)]
enum Constraint {
    Bbox(Bbox),
    Radius { lon: f64, lat: f64, radius: f64 },
    Polygon(Vec<crate::spatial::Point>),
}

/// The exact refinement a non-bbox spatial constraint applies after its bbox.
enum ExactFilter {
    Radius(crate::spatial::RadiusFilter),
    Polygon(crate::spatial::PolygonFilter),
}

impl ExactFilter {
    fn contains(&self, node: &osmflat::Node) -> bool {
        match self {
            Self::Radius(f) => f.contains(node),
            Self::Polygon(f) => f.contains(node),
        }
    }
}

/// A tag + spatial query over a verified [`crate::ExtArchive`], resolved to
/// ascending parent indices of one entity type.
///
/// Every constraint is ANDed. Tag postings are intersected smallest-first;
/// each spatial constraint is then applied as the ascending entity-index
/// ranges of its bbox (a range merge-join, design §3), and radius / polygon
/// constraints refine only the survivors by their exact test.
///
/// ```ignore
/// let cafes = archive
///     .query()
///     .with_tag("amenity", "cafe")
///     .with_tag("wheelchair", "yes")
///     .within_radius(-77.0365, 38.8977, 0.01)
///     .nodes()?;
/// ```
#[derive(Clone)]
pub struct Query<'a> {
    archive: &'a crate::ExtArchive,
    tags: Vec<(Vec<u8>, Vec<u8>)>,
    spatial: Vec<Constraint>,
}

impl<'a> Query<'a> {
    /// An empty query over `archive`. Prefer [`crate::ExtArchive::query`].
    pub fn new(archive: &'a crate::ExtArchive) -> Self {
        Self {
            archive,
            tags: Vec::new(),
            spatial: Vec::new(),
        }
    }

    /// Require the exact tag `key=value`.
    pub fn with_tag(mut self, key: impl AsRef<[u8]>, value: impl AsRef<[u8]>) -> Self {
        self.tags
            .push((key.as_ref().to_vec(), value.as_ref().to_vec()));
        self
    }

    /// Require the entity to fall in (nodes) or overlap (ways, relations)
    /// `bbox`, using osmflat's exact bbox query.
    pub fn in_bbox(mut self, bbox: Bbox) -> Self {
        self.spatial.push(Constraint::Bbox(bbox));
        self
    }

    /// Require the node to be within `radius` degrees of `(lon, lat)`,
    /// boundary included. Nodes only.
    pub fn within_radius(mut self, lon: f64, lat: f64, radius: f64) -> Self {
        self.spatial.push(Constraint::Radius { lon, lat, radius });
        self
    }

    /// Require the node to be inside `polygon` (a ring of `lon`/`lat` points),
    /// boundary included. Nodes only.
    pub fn in_polygon(mut self, polygon: &[crate::spatial::Point]) -> Self {
        self.spatial.push(Constraint::Polygon(polygon.to_vec()));
        self
    }

    /// Matching parent node indices, ascending.
    pub fn nodes(&self) -> Result<Vec<u64>, QueryError> {
        self.resolve(EntityType::Node)
    }

    /// Matching parent way indices, ascending.
    pub fn ways(&self) -> Result<Vec<u64>, QueryError> {
        self.resolve(EntityType::Way)
    }

    /// Matching parent relation indices, ascending.
    pub fn relations(&self) -> Result<Vec<u64>, QueryError> {
        self.resolve(EntityType::Relation)
    }

    fn resolve(&self, entity: EntityType) -> Result<Vec<u64>, QueryError> {
        if self.tags.is_empty() && self.spatial.is_empty() {
            return Err(QueryError::Unconstrained);
        }
        let parent = self.archive.parent();

        // Validate every spatial constraint before doing any work.
        let mut spatial = Vec::with_capacity(self.spatial.len());
        for constraint in &self.spatial {
            spatial.push(match constraint {
                Constraint::Bbox(bbox) => {
                    let finite = [bbox.min_lon, bbox.min_lat, bbox.max_lon, bbox.max_lat]
                        .iter()
                        .all(|v| v.is_finite());
                    if !finite || bbox.min_lon > bbox.max_lon || bbox.min_lat > bbox.max_lat {
                        return Err(QueryError::InvalidSpatial(
                            "in_bbox needs finite coordinates with min <= max",
                        ));
                    }
                    (*bbox, None)
                }
                Constraint::Radius { lon, lat, radius } => {
                    if entity != EntityType::Node {
                        return Err(QueryError::NodesOnly {
                            constraint: "within_radius",
                            entity,
                        });
                    }
                    let filter = crate::spatial::RadiusFilter::new(parent, *lon, *lat, *radius)
                        .ok_or(QueryError::InvalidSpatial(
                            "within_radius needs a finite center and a finite, non-negative radius",
                        ))?;
                    (filter.bbox, Some(ExactFilter::Radius(filter)))
                }
                Constraint::Polygon(polygon) => {
                    if entity != EntityType::Node {
                        return Err(QueryError::NodesOnly {
                            constraint: "in_polygon",
                            entity,
                        });
                    }
                    let filter = crate::spatial::PolygonFilter::new(parent, polygon).ok_or(
                        QueryError::InvalidSpatial(
                            "in_polygon needs at least 3 vertices with finite coordinates",
                        ),
                    )?;
                    (filter.bbox, Some(ExactFilter::Polygon(filter)))
                }
            });
        }

        let mut acc: Option<Vec<u64>> = None;
        if !self.tags.is_empty() {
            let taginfo = self.archive.taginfo().ok_or(QueryError::NoTaginfo)?;
            let mut selection = Selection::new();
            for (key, value) in &self.tags {
                // An absent tag matches nothing; skip the spatial work.
                let Some(view) = taginfo.kv(key, value) else {
                    return Ok(Vec::new());
                };
                selection = selection.and(match entity {
                    EntityType::Node => view.nodes(),
                    EntityType::Way => view.ways(),
                    EntityType::Relation => view.relations(),
                });
            }
            acc = Some(selection.resolve());
        }

        for (bbox, exact) in spatial {
            if acc.as_ref().is_some_and(Vec::is_empty) {
                return Ok(Vec::new());
            }
            let in_box = match entity {
                EntityType::Node => node_indices_in_bbox(parent, bbox),
                EntityType::Way => way_indices_in_bbox(parent, bbox),
                EntityType::Relation => relation_indices_in_bbox(parent, bbox),
            };
            let mut next = match acc.take() {
                None => in_box,
                Some(prev) => clip_to_ranges(&prev, &to_index_ranges(&in_box)),
            };
            if let Some(exact) = exact {
                let nodes = parent.nodes();
                next.retain(|&idx| exact.contains(&nodes[idx as usize]));
            }
            acc = Some(next);
        }

        Ok(acc.unwrap_or_default())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(vals: &[u64]) -> Vec<Ref> {
        vals.iter()
            .map(|&v| {
                let mut r = Ref::new();
                r.set_value(v);
                r
            })
            .collect()
    }

    /// The collect-sort-dedup oracle `union` replaced.
    fn union_oracle(lists: &[&[Ref]]) -> Vec<u64> {
        let mut out: Vec<u64> = lists
            .iter()
            .flat_map(|l| l.iter().map(|p| p.value()))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    #[test]
    fn union_of_nothing_is_empty() {
        assert_eq!(union(&[]).count(), 0);
        let empty = refs(&[]);
        assert_eq!(union(&[&empty, &empty]).count(), 0);
    }

    #[test]
    fn union_single_list_passes_through() {
        let a = refs(&[2, 5, 9]);
        assert_eq!(union(&[&a]).collect::<Vec<_>>(), vec![2, 5, 9]);
    }

    #[test]
    fn union_merges_ascending_and_dedups() {
        let a = refs(&[1, 3, 5]);
        let b = refs(&[2, 3, 6]);
        let c = refs(&[5, 7]);
        let empty = refs(&[]);
        assert_eq!(
            union(&[&a, &b, &c, &empty]).collect::<Vec<_>>(),
            vec![1, 2, 3, 5, 6, 7]
        );
    }

    #[test]
    fn union_is_lazy_prefix() {
        let a = refs(&[1, 4, 8]);
        let b = refs(&[2, 4, 9]);
        assert_eq!(union(&[&a, &b]).take(3).collect::<Vec<_>>(), vec![1, 2, 4]);
    }

    #[test]
    fn union_matches_sort_dedup_oracle() {
        // Deterministic pseudo-random ascending lists with heavy overlap.
        let mut state = 0x9e3779b97f4a7c15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let lists: Vec<Vec<Ref>> = (0..7)
            .map(|_| {
                let len = (next() % 40) as usize;
                let mut vals: Vec<u64> = (0..len).map(|_| next() % 64).collect();
                vals.sort_unstable();
                vals.dedup();
                refs(&vals)
            })
            .collect();
        let slices: Vec<&[Ref]> = lists.iter().map(Vec::as_slice).collect();
        assert_eq!(
            union(&slices).collect::<Vec<_>>(),
            union_oracle(&slices),
            "streaming union diverged from sort-dedup oracle"
        );
    }
}
