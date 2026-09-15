//! Non-bbox spatial queries (radius, k-NN, polygon).
//!
//! These need **no sidecar**: they build on the parent's existing
//! space-filling-curve order and its `find_*_by_bounding_box`. The functions
//! here return nearest-first or plain index lists; to combine a radius or
//! polygon with tag filters, use [`crate::query::Query`]
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

#[derive(Debug, Clone, Copy)]
struct ScaledPoint {
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

/// Exact point-in-polygon test for nodes (boundary included), plus the
/// polygon's bbox that prefilters it. Shared by [`nodes_in_polygon`] and
/// [`crate::query::Query`].
pub(crate) struct PolygonFilter {
    /// Bbox of the polygon; every matching node lies inside it.
    pub(crate) bbox: crate::query::Bbox,
    scaled: Vec<ScaledPoint>,
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
        Some(Self {
            bbox: polygon_bbox(polygon),
            scaled: polygon
                .iter()
                .map(|p| scale_point(p.lon, p.lat, coord_scale))
                .collect(),
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
