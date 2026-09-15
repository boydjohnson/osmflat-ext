//! Non-bbox spatial queries (radius, k-NN, polygon) for nodes, ways, and
//! relations.
//!
//! These need **no sidecar**: they build on the parent's existing
//! space-filling-curve order and its `find_*_by_bounding_box`. Ways and
//! relations match when **any part touches** the shape: any segment of a way,
//! or any member geometry of a relation, recursively. A closed way is a line
//! here, so it doesn't match a shape it merely encloses. All containment tests
//! are exact in archive units.
//!
//! The functions here return nearest-first or plain index lists; to combine a
//! radius or polygon with tag filters, use [`crate::query::Query`]
//! (`ExtArchive::query()`), which applies the same exact tests after a range
//! merge-join with the tag postings.

use osmflat::Osm;
use std::collections::{BinaryHeap, HashSet};

/// A geographic point in degrees (`lon` = x, `lat` = y).
#[derive(Debug, Clone, Copy)]
pub struct Point {
    pub lon: f64,
    pub lat: f64,
}

/// A point in archive units (degrees × `coord_scale`).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ScaledPoint {
    lon: i32,
    lat: i32,
}

/// Node indices within `radius` degrees of `center`, nearest-first.
///
/// Uses the parent archive's bbox query as a prefilter, then refines by exact
/// squared distance in archive coordinate units.
pub fn nodes_within_radius(
    archive: &Osm,
    center_lon: f64,
    center_lat: f64,
    radius: f64,
) -> impl Iterator<Item = usize> {
    let Some(filter) = RadiusFilter::new(archive, center_lon, center_lat, radius) else {
        return Vec::new().into_iter();
    };

    let mut matches: Vec<(i128, usize)> = crate::query::node_indices_in_bbox(archive, filter.bbox)
        .into_iter()
        .filter_map(|idx| {
            let idx = idx as usize;
            let dist = filter.distance_sq(&archive.nodes()[idx]);
            (dist <= filter.radius_sq).then_some((dist, idx))
        })
        .collect();

    matches.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    matches
        .into_iter()
        .map(|(_, idx)| idx)
        .collect::<Vec<_>>()
        .into_iter()
}

/// The `k` nearest node indices to `center`, nearest-first (ties by index).
///
/// Expanding-square search: query the parent's bbox index for a square of
/// half-width `r` around `center`, keeping the `k` best candidates in a bounded
/// heap, and double `r` until the `k`th-best distance is `<= r`. Every node
/// within distance `r` lies inside the square, so at that point no unvisited
/// node can displace a kept one and the result is exact. Memory is `O(k)`, and
/// the work is dominated by the final square rather than the whole archive.
pub fn k_nearest_nodes(archive: &Osm, center_lon: f64, center_lat: f64, k: usize) -> Vec<usize> {
    if k == 0 || !center_lon.is_finite() || !center_lat.is_finite() {
        return Vec::new();
    }

    let coord_scale = archive.header().coord_scale();
    let center = scale_point(center_lon, center_lat, coord_scale);
    let k = k.min(archive.nodes().len());
    if k == 0 {
        return Vec::new();
    }

    let (lon, lat) = (
        center.lon as f64 / coord_scale as f64,
        center.lat as f64 / coord_scale as f64,
    );
    let mut half_width = scale_radius(KNN_INITIAL_HALF_WIDTH_DEG, coord_scale).max(1) as i64;
    loop {
        // Pad by one archive unit so the degree-space edge filter in the bbox
        // query can't drop a node sitting exactly on the scaled square's edge.
        let reach = (half_width + 1) as f64 / coord_scale as f64;
        let bbox = crate::query::Bbox {
            min_lon: (lon - reach).max(-180.0),
            min_lat: (lat - reach).max(-90.0),
            max_lon: (lon + reach).min(180.0),
            max_lat: (lat + reach).min(90.0),
        };
        let covers_world = bbox.min_lon <= -180.0
            && bbox.min_lat <= -90.0
            && bbox.max_lon >= 180.0
            && bbox.max_lat >= 90.0;

        let best = nearest_in_bbox(archive, center, bbox, k);
        let settled = best.len() == k
            && best
                .peek()
                .is_some_and(|&(dist, _)| dist <= square(half_width));
        if settled || covers_world {
            let mut best = best.into_vec();
            best.sort_unstable();
            return best.into_iter().map(|(_, idx)| idx).collect();
        }
        half_width *= 2;
    }
}

/// Starting half-width of the k-NN search square, in degrees (~50 m of
/// latitude). Dense areas settle immediately; sparse ones double outward.
const KNN_INITIAL_HALF_WIDTH_DEG: f64 = 0.0005;

/// The `k` smallest `(distance_sq, index)` pairs among nodes in `bbox`, as a
/// max-heap (the worst kept candidate on top).
fn nearest_in_bbox(
    archive: &Osm,
    center: ScaledPoint,
    bbox: crate::query::Bbox,
    k: usize,
) -> BinaryHeap<(i128, usize)> {
    let nodes = archive.nodes();
    let mut best: BinaryHeap<(i128, usize)> = BinaryHeap::with_capacity(k + 1);
    // The bbox query may repeat a node across curve ranges; track what's kept.
    let mut kept: HashSet<usize> = HashSet::with_capacity(k + 1);

    for node in osmflat::find_nodes_by_bounding_box(
        archive,
        bbox.min_lon,
        bbox.min_lat,
        bbox.max_lon,
        bbox.max_lat,
    ) {
        let idx = crate::query::slice_index(nodes, node) as usize;
        let candidate = (
            distance_sq(
                center,
                ScaledPoint {
                    lon: node.lon(),
                    lat: node.lat(),
                },
            ),
            idx,
        );
        if best.len() == k && best.peek().is_some_and(|&worst| candidate >= worst) {
            continue;
        }
        if !kept.insert(idx) {
            continue;
        }
        best.push(candidate);
        if best.len() > k {
            if let Some((_, evicted)) = best.pop() {
                kept.remove(&evicted);
            }
        }
    }
    best
}

/// Node indices inside the `polygon` ring, in degrees (`lon` = x, `lat` = y).
///
/// Bbox-of-polygon prefilter via the spatial query, then exact point-in-polygon.
pub fn nodes_in_polygon(archive: &Osm, polygon: &[Point]) -> impl Iterator<Item = usize> {
    let Some(filter) = PolygonFilter::new(archive, polygon) else {
        return Vec::new().into_iter();
    };

    crate::query::node_indices_in_bbox(archive, filter.bbox)
        .into_iter()
        .filter_map(|idx| {
            let idx = idx as usize;
            filter.contains(&archive.nodes()[idx]).then_some(idx)
        })
        .collect::<Vec<_>>()
        .into_iter()
}

/// Exact "within `radius` degrees of a point" test for nodes, plus the bbox
/// that prefilters it. Shared by [`nodes_within_radius`] and
/// [`crate::query::Query`], so both agree on every edge case.
pub(crate) struct RadiusFilter {
    /// Square around the circle; every matching node lies inside it.
    pub(crate) bbox: crate::query::Bbox,
    center: ScaledPoint,
    radius_sq: i128,
}

impl RadiusFilter {
    /// `None` for a negative or non-finite radius or center.
    pub(crate) fn new(
        archive: &Osm,
        center_lon: f64,
        center_lat: f64,
        radius: f64,
    ) -> Option<Self> {
        if radius < 0.0 || !center_lon.is_finite() || !center_lat.is_finite() || !radius.is_finite()
        {
            return None;
        }
        let coord_scale = archive.header().coord_scale();
        let radius_scaled = scale_radius(radius, coord_scale);
        Some(Self {
            bbox: bbox_around(
                center_lon,
                center_lat,
                radius_scaled as f64 / coord_scale as f64,
            ),
            center: scale_point(center_lon, center_lat, coord_scale),
            radius_sq: square(radius_scaled as i64),
        })
    }

    #[inline]
    fn distance_sq(&self, node: &osmflat::Node) -> i128 {
        distance_sq(
            self.center,
            ScaledPoint {
                lon: node.lon(),
                lat: node.lat(),
            },
        )
    }

    #[inline]
    pub(crate) fn contains(&self, node: &osmflat::Node) -> bool {
        self.distance_sq(node) <= self.radius_sq
    }
}

/// Exact point-in-polygon test (boundary included), plus the polygon's bbox
/// that prefilters it. Shared by the `*_in_polygon` functions and
/// [`crate::query::Query`].
pub(crate) struct PolygonFilter {
    /// Bbox of the polygon; every matching entity overlaps it.
    pub(crate) bbox: crate::query::Bbox,
    scaled: Vec<ScaledPoint>,
    /// Corners of the polygon's bbox in archive units, for cheap segment rejection.
    scaled_min: ScaledPoint,
    scaled_max: ScaledPoint,
}

impl PolygonFilter {
    /// `None` for fewer than 3 vertices or any non-finite coordinate.
    pub(crate) fn new(archive: &Osm, polygon: &[Point]) -> Option<Self> {
        if polygon.len() < 3
            || polygon
                .iter()
                .any(|p| !p.lon.is_finite() || !p.lat.is_finite())
        {
            return None;
        }
        let coord_scale = archive.header().coord_scale();
        let scaled: Vec<ScaledPoint> = polygon
            .iter()
            .map(|p| scale_point(p.lon, p.lat, coord_scale))
            .collect();
        let corner = |pick: fn(i32, i32) -> i32| {
            scaled.iter().skip(1).fold(scaled[0], |acc, p| ScaledPoint {
                lon: pick(acc.lon, p.lon),
                lat: pick(acc.lat, p.lat),
            })
        };
        Some(Self {
            bbox: polygon_bbox(polygon),
            scaled_min: corner(std::cmp::min),
            scaled_max: corner(std::cmp::max),
            scaled,
        })
    }

    #[inline]
    pub(crate) fn contains(&self, node: &osmflat::Node) -> bool {
        point_in_polygon(
            ScaledPoint {
                lon: node.lon(),
                lat: node.lat(),
            },
            &self.scaled,
        )
    }
}

// ---------------------------------------------------------------------------
// Ways and relations: "any part touches" semantics
// ---------------------------------------------------------------------------
//
// A way is its resolved node sequence as a linestring (unresolvable node refs
// are dropped, as in `multipolygon::way_node_indices`); a single-node way is a
// point. A closed way is still a linestring here: it matches a shape its line
// touches, not a shape it merely encloses. A relation touches if any member
// does — member nodes, member ways, and member relations recursively (each
// relation visited once, so cycles terminate).
//
// Candidates come from osmflat's way / relation bbox queries (a way's
// recomputed bounding box, a relation's stored one), so a relation whose
// stored bbox doesn't cover its members can be missed.

/// A spatial predicate that can be tested against points and segments, in
/// archive units.
pub(crate) trait Shape {
    fn touches_point(&self, p: ScaledPoint) -> bool;
    fn touches_segment(&self, a: ScaledPoint, b: ScaledPoint) -> bool;
}

impl Shape for RadiusFilter {
    fn touches_point(&self, p: ScaledPoint) -> bool {
        distance_sq(self.center, p) <= self.radius_sq
    }

    fn touches_segment(&self, a: ScaledPoint, b: ScaledPoint) -> bool {
        segment_within(self.center, a, b, self.radius_sq)
    }
}

impl Shape for PolygonFilter {
    fn touches_point(&self, p: ScaledPoint) -> bool {
        point_in_polygon(p, &self.scaled)
    }

    fn touches_segment(&self, a: ScaledPoint, b: ScaledPoint) -> bool {
        if a.lon.max(b.lon) < self.scaled_min.lon
            || a.lon.min(b.lon) > self.scaled_max.lon
            || a.lat.max(b.lat) < self.scaled_min.lat
            || a.lat.min(b.lat) > self.scaled_max.lat
        {
            return false;
        }
        if point_in_polygon(a, &self.scaled) || point_in_polygon(b, &self.scaled) {
            return true;
        }
        // Both endpoints outside: the segment touches only by crossing an edge.
        let mut previous = self.scaled[self.scaled.len() - 1];
        for &current in &self.scaled {
            if segments_intersect(a, b, previous, current) {
                return true;
            }
            previous = current;
        }
        false
    }
}

/// A piece of an entity's geometry.
enum Part {
    Point(ScaledPoint),
    Way(usize),
}

/// The resolved vertices of way `way_idx`, in order.
fn way_vertices(archive: &Osm, way_idx: usize) -> impl Iterator<Item = ScaledPoint> + '_ {
    let refs = archive.ways()[way_idx].refs();
    let nodes_index = archive.nodes_index();
    let nodes = archive.nodes();
    (refs.start as usize..refs.end as usize)
        .filter_map(move |i| nodes_index[i].value())
        .map(move |n| node_point(&nodes[n as usize]))
}

#[inline]
fn node_point(node: &osmflat::Node) -> ScaledPoint {
    ScaledPoint {
        lon: node.lon(),
        lat: node.lat(),
    }
}

/// Visit every node and way reachable from relation `rel_idx` (member
/// relations expanded, each relation once) until `visit` returns `true`.
/// Returns whether it did.
fn any_relation_part(archive: &Osm, rel_idx: usize, mut visit: impl FnMut(Part) -> bool) -> bool {
    let members = archive.relation_members();
    let nodes = archive.nodes();
    let mut seen = HashSet::from([rel_idx]);
    let mut stack = vec![rel_idx];
    while let Some(r) = stack.pop() {
        for member in members.at(r) {
            let hit = match member {
                osmflat::RelationMembersRef::NodeMember(m) => m
                    .node_idx()
                    .is_some_and(|n| visit(Part::Point(node_point(&nodes[n as usize])))),
                osmflat::RelationMembersRef::WayMember(m) => {
                    m.way_idx().is_some_and(|w| visit(Part::Way(w as usize)))
                }
                osmflat::RelationMembersRef::RelationMember(m) => {
                    if let Some(child) = m.relation_idx() {
                        if seen.insert(child as usize) {
                            stack.push(child as usize);
                        }
                    }
                    false
                }
            };
            if hit {
                return true;
            }
        }
    }
    false
}

pub(crate) fn way_touches(archive: &Osm, way_idx: usize, shape: &impl Shape) -> bool {
    let mut vertices = way_vertices(archive, way_idx);
    let Some(mut previous) = vertices.next() else {
        return false;
    };
    let mut single = true;
    for current in vertices {
        single = false;
        if shape.touches_segment(previous, current) {
            return true;
        }
        previous = current;
    }
    single && shape.touches_point(previous)
}

pub(crate) fn relation_touches(archive: &Osm, rel_idx: usize, shape: &impl Shape) -> bool {
    any_relation_part(archive, rel_idx, |part| match part {
        Part::Point(p) => shape.touches_point(p),
        Part::Way(w) => way_touches(archive, w, shape),
    })
}

/// Squared distance from `center` to the nearest point of way `way_idx`, in
/// archive units; `None` if the way has no resolved nodes.
fn way_distance_sq(archive: &Osm, way_idx: usize, center: ScaledPoint) -> Option<f64> {
    let mut vertices = way_vertices(archive, way_idx);
    let mut previous = vertices.next()?;
    let mut best = distance_sq(center, previous) as f64;
    for current in vertices {
        best = best.min(segment_distance_sq(center, previous, current));
        previous = current;
    }
    Some(best)
}

/// Squared distance from `center` to the nearest part of relation `rel_idx`;
/// `None` if no member resolves to any geometry.
fn relation_distance_sq(archive: &Osm, rel_idx: usize, center: ScaledPoint) -> Option<f64> {
    let mut best: Option<f64> = None;
    any_relation_part(archive, rel_idx, |part| {
        let d = match part {
            Part::Point(p) => Some(distance_sq(center, p) as f64),
            Part::Way(w) => way_distance_sq(archive, w, center),
        };
        if let Some(d) = d {
            best = Some(best.map_or(d, |b| b.min(d)));
        }
        false
    });
    best
}

/// Way indices touching the circle of `radius` degrees around `center`,
/// nearest-first (ties by index). See the module notes on way geometry.
pub fn ways_within_radius(
    archive: &Osm,
    center_lon: f64,
    center_lat: f64,
    radius: f64,
) -> impl Iterator<Item = usize> {
    within_radius_by(
        archive,
        center_lon,
        center_lat,
        radius,
        crate::query::way_indices_in_bbox,
        |f, i| way_touches(archive, i, f),
        |c, i| way_distance_sq(archive, i, c),
    )
    .into_iter()
}

/// Relation indices with any member geometry touching the circle of `radius`
/// degrees around `center`, nearest-first (ties by index).
pub fn relations_within_radius(
    archive: &Osm,
    center_lon: f64,
    center_lat: f64,
    radius: f64,
) -> impl Iterator<Item = usize> {
    within_radius_by(
        archive,
        center_lon,
        center_lat,
        radius,
        crate::query::relation_indices_in_bbox,
        |f, i| relation_touches(archive, i, f),
        |c, i| relation_distance_sq(archive, i, c),
    )
    .into_iter()
}

fn within_radius_by(
    archive: &Osm,
    center_lon: f64,
    center_lat: f64,
    radius: f64,
    candidates: fn(&Osm, crate::query::Bbox) -> Vec<u64>,
    touches: impl Fn(&RadiusFilter, usize) -> bool,
    distance: impl Fn(ScaledPoint, usize) -> Option<f64>,
) -> Vec<usize> {
    let Some(filter) = RadiusFilter::new(archive, center_lon, center_lat, radius) else {
        return Vec::new();
    };
    let mut matches: Vec<(f64, usize)> = candidates(archive, filter.bbox)
        .into_iter()
        .map(|i| i as usize)
        .filter(|&i| touches(&filter, i))
        .filter_map(|i| distance(filter.center, i).map(|d| (d, i)))
        .collect();
    matches.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    matches.into_iter().map(|(_, i)| i).collect()
}

/// Way indices touching `polygon` (boundary included), ascending.
pub fn ways_in_polygon(archive: &Osm, polygon: &[Point]) -> impl Iterator<Item = usize> {
    let Some(filter) = PolygonFilter::new(archive, polygon) else {
        return Vec::new().into_iter();
    };
    crate::query::way_indices_in_bbox(archive, filter.bbox)
        .into_iter()
        .map(|i| i as usize)
        .filter(|&i| way_touches(archive, i, &filter))
        .collect::<Vec<_>>()
        .into_iter()
}

/// Relation indices with any member geometry touching `polygon`, ascending.
pub fn relations_in_polygon(archive: &Osm, polygon: &[Point]) -> impl Iterator<Item = usize> {
    let Some(filter) = PolygonFilter::new(archive, polygon) else {
        return Vec::new().into_iter();
    };
    crate::query::relation_indices_in_bbox(archive, filter.bbox)
        .into_iter()
        .map(|i| i as usize)
        .filter(|&i| relation_touches(archive, i, &filter))
        .collect::<Vec<_>>()
        .into_iter()
}

/// The `k` ways nearest to `center` (distance to the nearest point of the
/// way's line), nearest-first, ties by index. Same expanding-square search as
/// [`k_nearest_nodes`].
pub fn k_nearest_ways(archive: &Osm, center_lon: f64, center_lat: f64, k: usize) -> Vec<usize> {
    k_nearest_by(
        archive,
        center_lon,
        center_lat,
        k,
        crate::query::way_indices_in_bbox,
        |c, i| way_distance_sq(archive, i, c),
    )
}

/// The `k` relations nearest to `center` (distance to the nearest member
/// geometry), nearest-first, ties by index.
pub fn k_nearest_relations(
    archive: &Osm,
    center_lon: f64,
    center_lat: f64,
    k: usize,
) -> Vec<usize> {
    k_nearest_by(
        archive,
        center_lon,
        center_lat,
        k,
        crate::query::relation_indices_in_bbox,
        |c, i| relation_distance_sq(archive, i, c),
    )
}

/// `(distance_sq, index)` ordered by distance (`total_cmp`), then index.
#[derive(Clone, Copy, PartialEq)]
struct Candidate(f64, usize);

impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Candidate {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0
            .total_cmp(&other.0)
            .then_with(|| self.1.cmp(&other.1))
    }
}

fn k_nearest_by(
    archive: &Osm,
    center_lon: f64,
    center_lat: f64,
    k: usize,
    candidates: fn(&Osm, crate::query::Bbox) -> Vec<u64>,
    distance: impl Fn(ScaledPoint, usize) -> Option<f64>,
) -> Vec<usize> {
    if k == 0 || !center_lon.is_finite() || !center_lat.is_finite() {
        return Vec::new();
    }
    let coord_scale = archive.header().coord_scale();
    let center = scale_point(center_lon, center_lat, coord_scale);
    let (lon, lat) = (
        center.lon as f64 / coord_scale as f64,
        center.lat as f64 / coord_scale as f64,
    );

    let mut half_width = scale_radius(KNN_INITIAL_HALF_WIDTH_DEG, coord_scale).max(2) as i64;
    loop {
        let reach = (half_width + 1) as f64 / coord_scale as f64;
        let bbox = crate::query::Bbox {
            min_lon: (lon - reach).max(-180.0),
            min_lat: (lat - reach).max(-90.0),
            max_lon: (lon + reach).min(180.0),
            max_lat: (lat + reach).min(90.0),
        };
        let covers_world = bbox.min_lon <= -180.0
            && bbox.min_lat <= -90.0
            && bbox.max_lon >= 180.0
            && bbox.max_lat >= 90.0;

        // Candidates are distinct (the bbox helpers dedup), so no `kept` set.
        let mut best: BinaryHeap<Candidate> = BinaryHeap::with_capacity(k + 1);
        for idx in candidates(archive, bbox) {
            let Some(d) = distance(center, idx as usize) else {
                continue;
            };
            let candidate = Candidate(d, idx as usize);
            if best.len() == k && best.peek().is_some_and(|worst| candidate >= *worst) {
                continue;
            }
            best.push(candidate);
            if best.len() > k {
                best.pop();
            }
        }

        // An entity within distance `r` of the center has a point inside the
        // square, so its bbox overlaps it and it was a candidate. Distances
        // here are floating point, so settle one archive unit early.
        let settle = (half_width - 1) as f64;
        let settled = best.len() == k && best.peek().is_some_and(|w| w.0 <= settle * settle);
        if settled || covers_world {
            let mut best = best.into_vec();
            best.sort_unstable();
            return best.into_iter().map(|c| c.1).collect();
        }
        half_width *= 2;
    }
}

/// Whether the distance from `p` to segment `ab` is `<= sqrt(r_sq)`, exactly.
fn segment_within(p: ScaledPoint, a: ScaledPoint, b: ScaledPoint, r_sq: i128) -> bool {
    let (dx, dy) = (b.lon as i128 - a.lon as i128, b.lat as i128 - a.lat as i128);
    let (qx, qy) = (p.lon as i128 - a.lon as i128, p.lat as i128 - a.lat as i128);
    let len_sq = dx * dx + dy * dy;
    let dot = qx * dx + qy * dy;
    if len_sq == 0 || dot <= 0 {
        return distance_sq(p, a) <= r_sq;
    }
    if dot >= len_sq {
        return distance_sq(p, b) <= r_sq;
    }
    // Perpendicular distance^2 = cross^2 / len^2; compare without dividing.
    let cross = qx * dy - qy * dx;
    let cross_sq = cross.unsigned_abs().checked_mul(cross.unsigned_abs());
    let rhs = (r_sq as u128).checked_mul(len_sq as u128);
    match (cross_sq, rhs) {
        (Some(lhs), Some(rhs)) => lhs <= rhs,
        // Only reachable for segments and radii spanning most of the globe.
        _ => (cross as f64) * (cross as f64) <= r_sq as f64 * len_sq as f64,
    }
}

/// Squared distance from `p` to segment `ab`, as `f64` (for ordering).
fn segment_distance_sq(p: ScaledPoint, a: ScaledPoint, b: ScaledPoint) -> f64 {
    let (dx, dy) = (b.lon as i128 - a.lon as i128, b.lat as i128 - a.lat as i128);
    let (qx, qy) = (p.lon as i128 - a.lon as i128, p.lat as i128 - a.lat as i128);
    let len_sq = dx * dx + dy * dy;
    let dot = qx * dx + qy * dy;
    if len_sq == 0 || dot <= 0 {
        return distance_sq(p, a) as f64;
    }
    if dot >= len_sq {
        return distance_sq(p, b) as f64;
    }
    let cross = (qx * dy - qy * dx) as f64;
    cross * cross / len_sq as f64
}

/// Sign of the cross product `(b - a) x (c - a)`.
fn orientation(a: ScaledPoint, b: ScaledPoint, c: ScaledPoint) -> i8 {
    let cross = (b.lon as i128 - a.lon as i128) * (c.lat as i128 - a.lat as i128)
        - (b.lat as i128 - a.lat as i128) * (c.lon as i128 - a.lon as i128);
    cross.signum() as i8
}

/// Whether segments `p1p2` and `q1q2` share any point (touching and collinear
/// overlap included), exactly.
fn segments_intersect(p1: ScaledPoint, p2: ScaledPoint, q1: ScaledPoint, q2: ScaledPoint) -> bool {
    let (d1, d2) = (orientation(q1, q2, p1), orientation(q1, q2, p2));
    let (d3, d4) = (orientation(p1, p2, q1), orientation(p1, p2, q2));
    if d1 * d2 < 0 && d3 * d4 < 0 {
        return true;
    }
    (d1 == 0 && point_on_segment(p1, q1, q2))
        || (d2 == 0 && point_on_segment(p2, q1, q2))
        || (d3 == 0 && point_on_segment(q1, p1, p2))
        || (d4 == 0 && point_on_segment(q2, p1, p2))
}

fn bbox_around(center_lon: f64, center_lat: f64, radius: f64) -> crate::query::Bbox {
    crate::query::Bbox {
        min_lon: center_lon - radius,
        min_lat: center_lat - radius,
        max_lon: center_lon + radius,
        max_lat: center_lat + radius,
    }
}

fn polygon_bbox(polygon: &[Point]) -> crate::query::Bbox {
    let (mut min_lon, mut min_lat, mut max_lon, mut max_lat) = (
        polygon[0].lon,
        polygon[0].lat,
        polygon[0].lon,
        polygon[0].lat,
    );
    for point in &polygon[1..] {
        min_lon = min_lon.min(point.lon);
        min_lat = min_lat.min(point.lat);
        max_lon = max_lon.max(point.lon);
        max_lat = max_lat.max(point.lat);
    }

    crate::query::Bbox {
        min_lon,
        min_lat,
        max_lon,
        max_lat,
    }
}

#[inline]
fn scale_point(lon: f64, lat: f64, coord_scale: i32) -> ScaledPoint {
    ScaledPoint {
        lon: scale_coordinate(lon, coord_scale),
        lat: scale_coordinate(lat, coord_scale),
    }
}

#[inline]
fn scale_coordinate(value: f64, coord_scale: i32) -> i32 {
    (value * coord_scale as f64) as i32
}

#[inline]
fn scale_radius(radius: f64, coord_scale: i32) -> i32 {
    (radius * coord_scale as f64).ceil() as i32
}

#[inline]
fn square(value: i64) -> i128 {
    let value = value as i128;
    value * value
}

#[inline]
fn distance_sq(a: ScaledPoint, b: ScaledPoint) -> i128 {
    square(a.lon as i64 - b.lon as i64) + square(a.lat as i64 - b.lat as i64)
}

fn point_in_polygon(point: ScaledPoint, polygon: &[ScaledPoint]) -> bool {
    let mut inside = false;
    let mut previous = polygon[polygon.len() - 1];

    for &current in polygon {
        if point_on_segment(point, previous, current) {
            return true;
        }

        let y = point.lat as f64;
        let x = point.lon as f64;
        let yi = current.lat as f64;
        let yj = previous.lat as f64;

        if (yi > y) != (yj > y) {
            let xi = current.lon as f64;
            let xj = previous.lon as f64;
            let x_intersection = (xj - xi) * (y - yi) / (yj - yi) + xi;
            if x < x_intersection {
                inside = !inside;
            }
        }

        previous = current;
    }

    inside
}

fn point_on_segment(point: ScaledPoint, a: ScaledPoint, b: ScaledPoint) -> bool {
    let px = point.lon as i128;
    let py = point.lat as i128;
    let ax = a.lon as i128;
    let ay = a.lat as i128;
    let bx = b.lon as i128;
    let by = b.lat as i128;

    let cross = (px - ax) * (by - ay) - (py - ay) * (bx - ax);
    cross == 0 && px >= ax.min(bx) && px <= ax.max(bx) && py >= ay.min(by) && py <= ay.max(by)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(lon: i32, lat: i32) -> ScaledPoint {
        ScaledPoint { lon, lat }
    }

    #[test]
    fn segment_within_is_exact_at_the_boundary() {
        let (a, b) = (p(0, 0), p(10, 0));
        // Perpendicular foot inside the segment: distance 3.
        assert!(segment_within(p(5, 3), a, b, 9));
        assert!(!segment_within(p(5, 3), a, b, 8));
        // Beyond an endpoint: distance to that endpoint, 5 (3-4-5).
        assert!(segment_within(p(13, 4), a, b, 25));
        assert!(!segment_within(p(13, 4), a, b, 24));
        assert!(segment_within(p(-3, -4), a, b, 25));
        assert!(!segment_within(p(-3, -4), a, b, 24));
        // Diagonal segment, foot inside: (0,0)-(4,4), point (0,4), distance^2 8.
        assert!(segment_within(p(0, 4), p(0, 0), p(4, 4), 8));
        assert!(!segment_within(p(0, 4), p(0, 0), p(4, 4), 7));
        // Zero-length segment degenerates to a point.
        assert!(segment_within(p(3, 4), p(0, 0), p(0, 0), 25));
        assert!(!segment_within(p(3, 4), p(0, 0), p(0, 0), 24));
        // Globe-spanning coordinates don't overflow.
        let (w, e) = (
            p(-1_800_000_000, -900_000_000),
            p(1_800_000_000, 900_000_000),
        );
        assert!(segment_within(p(0, 0), w, e, 0));
        assert!(!segment_within(p(0, 1), w, e, 0));
    }

    #[test]
    fn segment_distance_matches_segment_within() {
        let (a, b) = (p(0, 0), p(10, 0));
        assert_eq!(segment_distance_sq(p(5, 3), a, b), 9.0);
        assert_eq!(segment_distance_sq(p(13, 4), a, b), 25.0);
        assert_eq!(segment_distance_sq(p(0, 4), p(0, 0), p(4, 4)), 8.0);
        assert_eq!(segment_distance_sq(p(3, 4), p(0, 0), p(0, 0)), 25.0);
    }

    #[test]
    fn segments_intersect_cases() {
        let x = |a, b, c, d| segments_intersect(a, b, c, d);
        // Proper crossing.
        assert!(x(p(0, 0), p(10, 10), p(0, 10), p(10, 0)));
        // Touching at an endpoint, and a T junction.
        assert!(x(p(0, 0), p(5, 5), p(5, 5), p(10, 0)));
        assert!(x(p(0, 0), p(10, 0), p(5, 0), p(5, 7)));
        // Collinear overlap, and collinear but disjoint.
        assert!(x(p(0, 0), p(10, 0), p(5, 0), p(15, 0)));
        assert!(!x(p(0, 0), p(4, 0), p(5, 0), p(15, 0)));
        // Parallel near-miss, and a crossing line that stops one unit short.
        assert!(!x(p(0, 0), p(10, 0), p(0, 1), p(10, 1)));
        assert!(!x(p(0, 0), p(10, 0), p(5, 1), p(5, 7)));
    }

    #[test]
    fn polygon_touches_segment_cases() {
        let square = PolygonFilter {
            bbox: crate::query::Bbox {
                min_lon: 0.0,
                min_lat: 0.0,
                max_lon: 0.0,
                max_lat: 0.0,
            },
            scaled: vec![p(0, 0), p(10, 0), p(10, 10), p(0, 10)],
            scaled_min: p(0, 0),
            scaled_max: p(10, 10),
        };
        // Endpoint inside; both outside but crossing; touching an edge;
        // touching a corner at an endpoint.
        assert!(square.touches_segment(p(5, 5), p(20, 20)));
        assert!(square.touches_segment(p(-5, 5), p(15, 5)));
        assert!(square.touches_segment(p(10, -5), p(10, 20)));
        assert!(square.touches_segment(p(10, 10), p(20, 20)));
        // Fully outside, rejected by the bbox check.
        assert!(!square.touches_segment(p(20, 0), p(30, 10)));
        // Diagonals x + y = c with both endpoints outside the square:
        // c = 11 cuts through it, c = 20 touches only the (10, 10) corner,
        // c = 21 passes just outside that corner (its bbox still overlaps).
        assert!(square.touches_segment(p(-5, 16), p(16, -5)));
        assert!(square.touches_segment(p(-5, 25), p(25, -5)));
        assert!(!square.touches_segment(p(-5, 26), p(26, -5)));
    }
}
