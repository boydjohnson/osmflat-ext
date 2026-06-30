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
/// Seed with the cell containing `center`, expand the bbox in rings via
/// `find_nodes_by_bounding_box`, refine by true distance, stop once the ring
/// boundary exceeds `radius`.
pub fn nodes_within_radius(
    _archive: &Osm,
    _center: ScaledPoint,
    _radius: i64,
) -> impl Iterator<Item = usize> {
    todo!("expanding-ring bbox search + exact-distance refine");
    #[allow(unreachable_code)]
    std::iter::empty()
}

/// The `k` nearest node indices to `center`, nearest-first.
///
/// Expanding-ring search; stop when the k-th best is closer than the next
/// ring's boundary.
pub fn k_nearest_nodes(_archive: &Osm, _center: ScaledPoint, _k: usize) -> Vec<usize> {
    todo!("expanding-ring k-NN with early termination on ring boundary")
}

/// Node indices inside the `polygon` (scaled ring, lon/lat).
///
/// Bbox-of-polygon prefilter via the spatial query, then exact point-in-polygon.
pub fn nodes_in_polygon(_archive: &Osm, _polygon: &[ScaledPoint]) -> impl Iterator<Item = usize> {
    todo!("bbox prefilter via find_nodes_by_bounding_box, then point-in-polygon refine");
    #[allow(unreachable_code)]
    std::iter::empty()
}
