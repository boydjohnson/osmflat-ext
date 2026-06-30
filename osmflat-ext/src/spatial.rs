//! Non-bbox spatial queries (radius, k-NN, polygon).
//!
//! These need **no sidecar**: they build on the parent's existing
//! space-filling-curve order and its `find_*_by_bounding_box`. They live here
//! so they compose with tag/backref selections (a spatial candidate set is just
//! more ascending entity-index ranges to feed [`crate::query`]).

use osmflat::Osm;

/// A geographic point in scaled archive coordinates (`* header.coord_scale`).
#[derive(Debug, Clone, Copy)]
pub struct ScaledPoint {
    pub lon: i32,
    pub lat: i32,
}

/// Node indices within `radius` (scaled units) of `center`, nearest-first.
///
/// Uses the parent archive's bbox query as a prefilter, then refines by exact
/// squared distance in archive coordinate units.
pub fn nodes_within_radius(
    archive: &Osm,
    center: ScaledPoint,
    radius: i64,
) -> impl Iterator<Item = usize> {
    if radius < 0 {
        return Vec::new().into_iter();
    }

    let radius_sq = square(radius);
    let bbox = bbox_around(center, radius, archive.header().coord_scale());
    let mut matches: Vec<(i128, usize)> = crate::query::node_indices_in_bbox(archive, bbox)
        .into_iter()
        .filter_map(|idx| {
            let idx = idx as usize;
            let node = &archive.nodes()[idx];
            let dist = distance_sq(
                center,
                ScaledPoint {
                    lon: node.lon(),
                    lat: node.lat(),
                },
            );
            (dist <= radius_sq).then_some((dist, idx))
        })
        .collect();

    matches.sort_by_key(|&(dist, idx)| (dist, idx));
    matches
        .into_iter()
        .map(|(_, idx)| idx)
        .collect::<Vec<_>>()
        .into_iter()
}

/// The `k` nearest node indices to `center`, nearest-first.
///
/// This exact implementation scans all nodes and sorts by distance; the
/// expanding-ring optimization can replace it without changing results.
pub fn k_nearest_nodes(archive: &Osm, center: ScaledPoint, k: usize) -> Vec<usize> {
    if k == 0 {
        return Vec::new();
    }

    let mut nodes: Vec<(i128, usize)> = archive
        .nodes()
        .iter()
        .enumerate()
        .map(|(idx, node)| {
            (
                distance_sq(
                    center,
                    ScaledPoint {
                        lon: node.lon(),
                        lat: node.lat(),
                    },
                ),
                idx,
            )
        })
        .collect();

    nodes.sort_by_key(|&(dist, idx)| (dist, idx));
    nodes.into_iter().take(k).map(|(_, idx)| idx).collect()
}

/// Node indices inside the `polygon` (scaled ring, lon/lat).
///
/// Bbox-of-polygon prefilter via the spatial query, then exact point-in-polygon.
pub fn nodes_in_polygon(archive: &Osm, polygon: &[ScaledPoint]) -> impl Iterator<Item = usize> {
    if polygon.len() < 3 {
        return Vec::new().into_iter();
    }

    let bbox = polygon_bbox(polygon, archive.header().coord_scale());
    crate::query::node_indices_in_bbox(archive, bbox)
        .into_iter()
        .filter_map(|idx| {
            let idx = idx as usize;
            let node = &archive.nodes()[idx];
            point_in_polygon(
                ScaledPoint {
                    lon: node.lon(),
                    lat: node.lat(),
                },
                polygon,
            )
            .then_some(idx)
        })
        .collect::<Vec<_>>()
        .into_iter()
}

fn bbox_around(center: ScaledPoint, radius: i64, coord_scale: i32) -> crate::query::Bbox {
    crate::query::Bbox {
        min_lon: scaled_to_degrees((center.lon as i64).saturating_sub(radius), coord_scale),
        min_lat: scaled_to_degrees((center.lat as i64).saturating_sub(radius), coord_scale),
        max_lon: scaled_to_degrees((center.lon as i64).saturating_add(radius), coord_scale),
        max_lat: scaled_to_degrees((center.lat as i64).saturating_add(radius), coord_scale),
    }
}

fn polygon_bbox(polygon: &[ScaledPoint], coord_scale: i32) -> crate::query::Bbox {
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
        min_lon: scaled_to_degrees(min_lon as i64, coord_scale),
        min_lat: scaled_to_degrees(min_lat as i64, coord_scale),
        max_lon: scaled_to_degrees(max_lon as i64, coord_scale),
        max_lat: scaled_to_degrees(max_lat as i64, coord_scale),
    }
}

#[inline]
fn scaled_to_degrees(value: i64, coord_scale: i32) -> f64 {
    value as f64 / coord_scale as f64
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
