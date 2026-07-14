//! Global coastline ring assembly: stitches *all* `natural=coastline` ways in
//! an archive into closed rings, independent of relations (coastline is a
//! plain way tag, not typically relation-based), and classifies each ring's
//! interior as land or water by its winding direction.
//!
//! This mirrors what `osmcoastline` does for the standard OSM rendering
//! pipeline: assemble every coastline way in the dataset into rings once,
//! offline, rather than leaving renderers to either re-derive it live or (as
//! this project found across several real bays and rivers) simply have no
//! fill at all for anything that's only ever been mapped as a coastline line.
//!
//! OSM's coastline convention is fixed regardless of what a ring encloses:
//! walking a `natural=coastline` way in its stored direction, land is always
//! on the left, sea always on the right. For a simple closed ring, the
//! interior is on the left of a counter-clockwise traversal (a standard
//! result), so: a CCW ring's interior is land (the usual case -- an island's
//! or continent's outer boundary); a CW ring's interior is water entirely
//! enclosed by coastline (rarer -- a large inland sea or lake mapped this
//! way). Rings are stored sorted by enclosed area descending, so a painter's-
//! algorithm renderer that draws land rings in that order (largest first)
//! reconstructs arbitrarily deep nesting -- an island in a bay in a sea in a
//! larger bay -- correctly without needing explicit hole/exterior pairing at
//! all, the same `order=way_area` trick this project already uses elsewhere.

use crate::multipolygon::{assemble_rings_and_open_chains, way_node_indices, Vertex};
use osmflat::{find_tag, Osm};

/// One assembled coastline ring: its vertices plus whether its interior is
/// land (`true`) or water enclosed entirely by coastline (`false`).
pub struct CoastlineRing {
    pub is_land: bool,
    /// Enclosed area in approximate square meters (equirectangular
    /// approximation, adequate for sorting/classification; not geodesically
    /// exact).
    pub area_m2: f64,
    pub vertices: Vec<Vertex>,
}

/// Approximate signed area of a closed ring in square meters, via the
/// shoelace formula on an equirectangular projection centered on the ring's
/// mean latitude (adequate at ring scale; matches the same approximation
/// `multipolygon`'s `dist_m` uses for ring-closing distances). Positive means
/// counter-clockwise -- i.e. the interior is land, per the coastline
/// left-hand convention described in the module docs. Takes plain `(lon,
/// lat)` pairs so both the assembler (working in `Vertex`) and the
/// precomputed-sidecar reader (working in resolved coordinates, no node
/// index) can share it.
pub fn signed_area_m2(ring: &[(f64, f64)]) -> f64 {
    const R: f64 = 6_378_137.0;
    if ring.len() < 3 {
        return 0.0;
    }
    let mean_lat = ring.iter().map(|&(_, lat)| lat).sum::<f64>() / ring.len() as f64;
    let cos_lat = mean_lat.to_radians().cos();
    let xy = |&(lon, lat): &(f64, f64)| -> (f64, f64) {
        (lon.to_radians() * cos_lat * R, lat.to_radians() * R)
    };
    let mut sum = 0.0;
    for i in 0..ring.len() {
        let (x0, y0) = xy(&ring[i]);
        let (x1, y1) = xy(&ring[(i + 1) % ring.len()]);
        sum += x0 * y1 - x1 * y0;
    }
    sum / 2.0
}

/// Collect every `natural=coastline` way in the archive as an assembly
/// segment (parent node index + resolved `(lon, lat)` per vertex).
fn collect_coastline_segments(archive: &Osm, scale: f64) -> Vec<Vec<Vertex>> {
    let nodes = archive.nodes();
    archive
        .ways()
        .iter()
        .filter(|way| find_tag(archive, way.tags(), b"natural") == Some(b"coastline"))
        .filter_map(|way| {
            let idx = way_node_indices(archive, &way);
            if idx.len() < 2 {
                return None;
            }
            Some(
                idx.iter()
                    .map(|&n| {
                        let node = &nodes[n as usize];
                        Vertex {
                            node_idx: n,
                            lon: node.lon() as f64 / scale,
                            lat: node.lat() as f64 / scale,
                        }
                    })
                    .collect(),
            )
        })
        .collect()
}

/// Classify a point as land (`true`) or water (`false`) using a signed
/// crossing-number test against every `natural=coastline` way's segments in
/// the archive directly -- **no ring assembly at all**, unlike
/// [`assemble_coastline`]. That's the point: a mainland coastline chain never
/// closes into a ring (it runs off toward an inland border with no
/// coastline tag at all -- see [`assemble_coastline_raw`]'s docs, and the
/// project's notes on why frame-closing against a rectangle doesn't work
/// here, since real extracts are clipped to a country's actual shape, not a
/// rectangle). But each individual segment still correctly encodes "land on
/// the left, sea on the right" regardless of whether it happens to be part
/// of a closed ring, and a signed crossing count is insensitive to that --
/// it works uniformly for islands *and* mainland.
///
/// Casts a ray due east from `(lon, lat)` at fixed latitude. For each
/// segment crossing that ray to the east of the point: a segment heading
/// north (increasing latitude) contributes `+1` (land, being on the
/// segment's left/west side, is on the point's side); a segment heading
/// south contributes `-1`. A positive net count means land wraps around the
/// point net counter-clockwise (the same sense as a CCW land ring in
/// [`assemble_coastline`]); zero or negative means water -- zero because
/// nothing encloses the point at all (open sea), negative because it's
/// inside a CW ring (water enclosed by coastline, e.g. a large inland sea).
pub fn is_land(archive: &Osm, scale: f64, lon: f64, lat: f64) -> bool {
    classify_points(archive, scale, &[(lon, lat)])[0]
}

/// Like [`is_land`], but classifies many points in one pass over the
/// archive's coastline ways instead of rescanning it once per point --
/// worth using whenever more than one point is needed (e.g. sampling a
/// render query's corners).
pub fn classify_points(archive: &Osm, scale: f64, points: &[(f64, f64)]) -> Vec<bool> {
    let nodes = archive.nodes();
    let mut winding = vec![0i64; points.len()];
    for way in archive.ways().iter() {
        if find_tag(archive, way.tags(), b"natural") != Some(b"coastline") {
            continue;
        }
        let idx = way_node_indices(archive, &way);
        let resolve = |n: u64| -> (f64, f64) {
            let node = &nodes[n as usize];
            (node.lon() as f64 / scale, node.lat() as f64 / scale)
        };
        for w in idx.windows(2) {
            let (a, b) = (resolve(w[0]), resolve(w[1]));
            for (i, &(lon, lat)) in points.iter().enumerate() {
                winding[i] += crossing_contribution(a, b, lon, lat);
            }
        }
    }
    winding.into_iter().map(|w| w > 0).collect()
}

/// Contribution of directed segment `a -> b` to the signed crossing count
/// for a ray cast due east from `(lon, lat)`; 0 if the segment doesn't cross
/// that ray to the east of the point. Uses a half-open latitude interval
/// (`[lo, hi)`) so a ray passing exactly through a vertex shared by two
/// segments is counted by exactly one of them, not zero or two.
fn crossing_contribution(a: (f64, f64), b: (f64, f64), lon: f64, lat: f64) -> i64 {
    let (upward, lo, hi) = if a.1 < b.1 {
        (true, a, b)
    } else {
        (false, b, a)
    };
    if lo.1 == hi.1 || !(lo.1 <= lat && lat < hi.1) {
        return 0; // horizontal segment, or doesn't span this latitude
    }
    let t = (lat - lo.1) / (hi.1 - lo.1);
    let crossing_lon = lo.0 + t * (hi.0 - lo.0);
    if crossing_lon <= lon {
        return 0; // crossing is west of the point, not on the eastward ray
    }
    if upward {
        1
    } else {
        -1
    }
}

/// Assemble every `natural=coastline` way in the archive into closed rings,
/// classified land/water and sorted by enclosed area descending, *and* the
/// leftover open chains that didn't close on their own -- a plain mainland
/// coastline (as opposed to an island's) never closes this way, since it
/// necessarily runs off toward an inland border with no coastline tag at
/// all, not because of a data error. [`frame_close`] can turn qualifying open
/// chains (both endpoints near a bounding frame) into additional rings;
/// callers that don't need that can just use [`assemble_coastline`].
pub fn assemble_coastline_raw(archive: &Osm, scale: f64) -> (Vec<CoastlineRing>, Vec<Vec<Vertex>>) {
    let segments = collect_coastline_segments(archive, scale);
    let assembled = assemble_rings_and_open_chains(segments);

    let mut rings: Vec<CoastlineRing> = assembled
        .rings
        .into_iter()
        .map(|vertices| {
            let xy: Vec<(f64, f64)> = vertices.iter().map(|v| (v.lon, v.lat)).collect();
            let signed = signed_area_m2(&xy);
            CoastlineRing {
                is_land: signed > 0.0,
                area_m2: signed.abs(),
                vertices,
            }
        })
        .collect();
    rings.sort_unstable_by(|a, b| b.area_m2.partial_cmp(&a.area_m2).unwrap());
    (rings, assembled.open_chains)
}

/// Assemble every `natural=coastline` way in the archive into closed rings,
/// classified land/water and sorted by enclosed area descending. Leftover
/// open chains (most commonly a mainland coastline, which can never close on
/// its own -- see [`assemble_coastline_raw`]) are silently dropped; there's
/// no meaningful fill for an unclosed coastline without frame-closing.
pub fn assemble_coastline(archive: &Osm, scale: f64) -> Vec<CoastlineRing> {
    assemble_coastline_raw(archive, scale).0
}

/// One precomputed coastline ring, read with no re-assembly: vertices already
/// resolved to `(lon, lat)` via the parent's own `nodes` vector.
pub struct CoastlineRingView {
    pub is_land: bool,
    /// Enclosed area in approximate square meters, recomputed from the
    /// resolved vertices (not stored in the sidecar -- see the module docs
    /// for why only node indices are persisted).
    pub area_m2: f64,
    pub vertices: Vec<(f64, f64)>,
}

/// Query API over a precomputed `Coastline` sub-archive: reads the rings
/// [`build_coastline`](../../osmflat-extc) wrote, with no live re-assembly.
#[derive(Clone, Copy)]
pub struct CoastlineQuery<'a> {
    parent: &'a Osm,
    coastline: &'a crate::Coastline,
}

impl<'a> CoastlineQuery<'a> {
    /// Wrap a parent archive and its `Coastline` sub-archive. Prefer
    /// [`crate::ExtArchive::coastline`], which verifies the fingerprint first.
    #[inline]
    pub fn new(parent: &'a Osm, coastline: &'a crate::Coastline) -> Self {
        Self { parent, coastline }
    }

    /// All precomputed rings, in stored order (enclosed area descending --
    /// see the module docs for why that order lets a plain painter's-
    /// algorithm renderer nest islands-in-bays-in-seas correctly).
    pub fn rings(&self) -> Vec<CoastlineRingView> {
        let scale = self.parent.header().coord_scale() as f64;
        let nodes = self.parent.nodes();
        let node_refs = self.coastline.nodes();

        self.coastline
            .rings()
            .iter()
            .map(|entry| {
                let r = entry.nodes();
                let vertices: Vec<(f64, f64)> = node_refs[r.start as usize..r.end as usize]
                    .iter()
                    .map(|nr| {
                        let n = &nodes[nr.value() as usize];
                        (n.lon() as f64 / scale, n.lat() as f64 / scale)
                    })
                    .collect();
                CoastlineRingView {
                    is_land: entry.is_land() != 0,
                    area_m2: signed_area_m2(&vertices).abs(),
                    vertices,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ccw_ring_is_classified_as_land() {
        // A simple square traversed counter-clockwise.
        let ring = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)];
        assert!(
            signed_area_m2(&ring) > 0.0,
            "CCW ring must have positive signed area"
        );
    }

    #[test]
    fn cw_ring_is_classified_as_water() {
        // The same square traversed clockwise (reversed).
        let ring = vec![(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0), (0.0, 0.0)];
        assert!(
            signed_area_m2(&ring) < 0.0,
            "CW ring must have negative signed area"
        );
    }

    /// Sum of `crossing_contribution` over a ring/chain's consecutive edges
    /// -- the same computation `is_land` does per way, without needing an
    /// `Osm` archive at all.
    fn winding(segments: &[(f64, f64)], lon: f64, lat: f64) -> i64 {
        segments
            .windows(2)
            .map(|w| crossing_contribution(w[0], w[1], lon, lat))
            .sum()
    }

    #[test]
    fn ray_cast_land_inside_ccw_ring() {
        let ring = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)];
        assert!(winding(&ring, 0.5, 0.5) > 0, "center of a CCW ring is land");
    }

    #[test]
    fn ray_cast_water_outside_ring() {
        let ring = vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)];
        assert!(winding(&ring, 1.5, 0.5) <= 0, "east of the ring is water");
        assert!(winding(&ring, -0.5, 0.5) <= 0, "west of the ring is water");
    }

    #[test]
    fn ray_cast_water_inside_cw_ring() {
        // An enclosed sea: same square, opposite (CW) winding.
        let ring = vec![(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0), (0.0, 0.0)];
        assert!(
            winding(&ring, 0.5, 0.5) <= 0,
            "center of a CW ring is water"
        );
    }

    #[test]
    fn ray_cast_lake_inside_island() {
        // A CCW island (0,0)-(10,10) with a CW lake (2,2)-(3,3) carved out.
        let island = [
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
            (0.0, 0.0),
        ];
        let lake = [(2.0, 2.0), (2.0, 3.0), (3.0, 3.0), (3.0, 2.0), (2.0, 2.0)];
        let total_at = |lon: f64, lat: f64| winding(&island, lon, lat) + winding(&lake, lon, lat);

        assert!(
            total_at(5.0, 5.0) > 0,
            "island interior, outside the lake, is land"
        );
        assert!(
            total_at(2.5, 2.5) <= 0,
            "inside the lake (surrounded by land) is water"
        );
        assert!(
            total_at(15.0, 5.0) <= 0,
            "outside the island entirely is water"
        );
    }

    #[test]
    fn ray_cast_open_chain_land_on_left_water_on_right() {
        // An open (never-closed) chain -- the mainland case frame-closing
        // couldn't handle -- a single diagonal segment (0,0)-(10,10) heading
        // northeast. Per "land on the left": heading NE, left is NW, so land
        // is on the NW side of the line y=x, sea on the SE side. A ray cast
        // at a fixed latitude only crosses a segment that actually spans
        // that latitude, so both test points are chosen within the chain's
        // own [0, 10) latitude span (matching how, on real data, this works
        // because the *global* coastline spans the full range of latitudes
        // involved, not because any single short segment does).
        let chain = vec![(0.0, 0.0), (10.0, 10.0)];
        assert!(
            winding(&chain, 2.0, 8.0) > 0,
            "NW of the line (y=x) is land"
        );
        assert!(
            winding(&chain, 8.0, 2.0) <= 0,
            "SE of the line (y=x) is water"
        );
    }

    // A full archive-backed test (assemble_coastline against a real `Osm`
    // archive, split across multiple ways to exercise real stitching, with
    // both a land and a water-enclosed ring) lives in osmflat-extc's test
    // suite, which already has the Fixture/build_parent_archive test-support
    // helpers this needs -- see tests/coastline.rs. Adding a second,
    // hand-rolled archive builder here would just be a second, less-tested
    // copy of that machinery.
}
