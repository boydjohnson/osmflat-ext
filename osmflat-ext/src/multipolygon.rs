//! Shared multipolygon ring assembly: stitches a `type=multipolygon`/
//! `boundary` relation's outer/inner member ways into closed rings.
//!
//! Used by two different callers against the same parent archive:
//! - `osmflat-mapnik-plugin`'s live rendering path, which flattens straight to
//!   `(lon, lat)` coordinates for immediate rendering (see its `materialize_relation`);
//! - `osmflat-extc`'s `--multipolygons` sidecar builder, which instead keeps
//!   [`Vertex::node_idx`] so the assembled rings can be stored once (as parent
//!   node indices, not duplicated coordinates) and reused at query time
//!   without re-stitching.
//!
//! This mirrors what a batch tool like `osmcoastline` does for coastlines in
//! the wider OSM rendering ecosystem: assemble once, offline, with room to
//! reason about ambiguous cases, rather than on every render query. Live
//! per-query assembly is more fragile than it looks -- this exact algorithm
//! had a real premature-ring-closure bug (see `ring_assembler_prefers_continuing_over_premature_close`
//! below) that only showed up on a real, jagged coastline.

use crate::Ref;
use osmflat::{find_tag, Osm, Relation, RelationMembersRef, Way};

/// A ring/chain vertex carried through assembly: the parent node index (what
/// a caller wanting to *store* a ring keeps) alongside its resolved
/// `(lon, lat)` (what the distance-based stitching itself needs).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub node_idx: u64,
    pub lon: f64,
    pub lat: f64,
}

impl Vertex {
    #[inline]
    fn xy(self) -> (f64, f64) {
        (self.lon, self.lat)
    }
}

/// True if the relation is an area type whose member ways enclose polygons.
pub fn is_area_relation(archive: &Osm, relation: &Relation) -> bool {
    match find_tag(archive, relation.tags(), b"type") {
        Some(v) => v == b"multipolygon" || v == b"boundary",
        None => false,
    }
}

/// Resolve a way's node-index sequence (dropping unresolved refs).
pub fn way_node_indices(archive: &Osm, way: &Way) -> Vec<u64> {
    let nodes_index = archive.nodes_index();
    let refs = way.refs();
    (refs.start as usize..refs.end as usize)
        .filter_map(|i| nodes_index[i].value())
        .collect()
}

/// Endpoints within this distance are treated as the same junction when
/// stitching ring segments. Chosen to bridge duplicate-node seams seen in
/// real extracts (tens to a couple hundred meters) while staying well below
/// the gap left by genuinely missing boundary members (800m+).
const RING_SNAP_TOLERANCE_M: f64 = 250.0;

/// Approximate great-circle distance in meters between two `(lon, lat)`
/// points in degrees. Equirectangular approximation; adequate at the scale
/// of ring-closing gaps (tens to hundreds of meters).
fn dist_m(a: (f64, f64), b: (f64, f64)) -> f64 {
    const R: f64 = 6_378_137.0;
    let (lon1, lat1) = (a.0.to_radians(), a.1.to_radians());
    let (lon2, lat2) = (b.0.to_radians(), b.1.to_radians());
    let x = (lon2 - lon1) * ((lat1 + lat2) / 2.0).cos();
    let y = lat2 - lat1;
    R * (x * x + y * y).sqrt()
}

/// Endpoints closer than this are treated as identical (floating-point
/// round-trip noise only) — used to detect a way that is already closed on
/// its own, which must not be confused with the much larger snap tolerance
/// used for bridging duplicate-node seams between *different* ways.
const EXACT_EPS_M: f64 = 0.01;

pub struct AssembledRings {
    pub rings: Vec<Vec<Vertex>>,
    pub open_chains: Vec<Vec<Vertex>>,
}

/// Stitch open/closed member segments (each a node sequence) into closed
/// rings by matching endpoints, exactly or (once at least one seam between
/// two distinct ways has been stitched) within `RING_SNAP_TOLERANCE_M`
/// (real-world extracts sometimes encode the same junction as two distinct,
/// near-coincident nodes). A lone way is only accepted as its own ring if its
/// ends are exactly coincident — otherwise a short way whose two ends simply
/// happen to be near each other would be misread as a closed area. Leftovers
/// with a gap too large to bridge (e.g. a member way missing from a clipped
/// extract) are returned as open chains.
pub fn assemble_rings_and_open_chains(mut segments: Vec<Vec<Vertex>>) -> AssembledRings {
    let mut rings = Vec::new();
    let mut open_chains = Vec::new();
    while let Some(mut ring) = segments.pop() {
        let mut stitched = 0u32;
        loop {
            let close_dist = dist_m(ring.first().unwrap().xy(), ring.last().unwrap().xy());
            let end = ring.last().unwrap().xy();

            // Find the remaining segment whose near end is closest to this open
            // endpoint (not merely the first one within tolerance): distinct
            // rings that pass close to each other (e.g. the main shoreline and a
            // separate ring around a harbor mouth) can each have an endpoint
            // within RING_SNAP_TOLERANCE_M, and picking the first match in list
            // order risks splicing the wrong ring in.
            let next = segments
                .iter()
                .enumerate()
                .filter_map(|(i, s)| {
                    let d = dist_m(s.first().unwrap().xy(), end)
                        .min(dist_m(s.last().unwrap().xy(), end));
                    (d < RING_SNAP_TOLERANCE_M).then_some((i, d))
                })
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap());

            // An exact self-closure (a lone way that's already its own ring,
            // or the true final seam landing back on the first vertex) always
            // wins outright -- no ambiguity to weigh.
            let exact_closed = ring.len() > 1 && close_dist < EXACT_EPS_M;

            // Past that, closing is only safe to prefer once at least one
            // stitch has happened (so this isn't just a short way whose ends
            // happen to be near each other) *and* it's actually the better
            // move: a jagged real shoreline can easily wander back within
            // RING_SNAP_TOLERANCE_M of its own start long before the ring is
            // truly finished, while a segment is still sitting right there
            // that plainly continues it. Treating "close enough" as an
            // unconditional early-out (the old behavior) grabbed that
            // coincidence over the real continuation, producing a bogus small
            // ring plus an orphaned leftover chain instead of the one correct,
            // larger ring -- so only close here if nothing closer is
            // available to extend with instead.
            let loose_closed = ring.len() > 1
                && stitched > 0
                && close_dist < RING_SNAP_TOLERANCE_M
                && next.is_none_or(|(_, d)| close_dist <= d);

            if exact_closed || loose_closed {
                rings.push(ring);
                break;
            }

            match next {
                Some((i, _)) => {
                    let mut seg = segments.remove(i);
                    if dist_m(seg.last().unwrap().xy(), end)
                        < dist_m(seg.first().unwrap().xy(), end)
                    {
                        seg.reverse();
                    }
                    // seg now starts near `end`; append the rest, skipping its matched vertex.
                    ring.extend_from_slice(&seg[1..]);
                    stitched += 1;
                }
                None => {
                    open_chains.push(ring);
                    break;
                }
            }
        }
    }
    let rings = rings.into_iter().flat_map(split_self_touching).collect();
    AssembledRings { rings, open_chains }
}

/// Split a ring that touches itself at a node into the separate rings it
/// actually describes.
///
/// Two exclaves of the same boundary relation can meet at a single shared
/// node (Baarle-Hertog has exactly this: a 1.65 km² exclave and a 0.024 km²
/// one joined at one corner). Stitching alone -- whether by endpoint distance
/// or by exact node identity -- walks straight through that node and produces
/// one figure-eight ring instead of two, because at the junction "keep going"
/// and "close here" are both locally valid. Osmium's area assembler splits at
/// such a node, and osmium's output is what `compare_plugins` diffs against:
/// splitting the figure-eight reproduces its two areas exactly, while leaving
/// it joined loses one polygon and understates the pair's combined area
/// (the two traversals partially cancel in the shoelace sum).
///
/// A ring with no repeated node is returned unchanged, so this only affects
/// the self-touching case. Pieces too small to enclose area (a spike that
/// doubles back on one node) are dropped.
fn split_self_touching(ring: Vec<Vertex>) -> Vec<Vec<Vertex>> {
    // A ring closed on its own first node repeats it at the end by
    // construction; that repeat is the closure, not a self-touch.
    let explicitly_closed =
        ring.len() > 1 && ring.first().unwrap().node_idx == ring.last().unwrap().node_idx;
    let body = if explicitly_closed {
        &ring[..ring.len() - 1]
    } else {
        &ring[..]
    };

    // Common case first: nothing repeats, so the ring stands as assembled.
    let mut distinct = std::collections::HashSet::with_capacity(body.len());
    if body.iter().all(|v| distinct.insert(v.node_idx)) {
        return vec![ring];
    }

    // Walk the ring keeping the path so far; revisiting a node means
    // everything since that node forms a closed loop of its own. The junction
    // node itself stays on the path -- it belongs to both rings.
    let mut seen: std::collections::HashMap<u64, usize> = std::collections::HashMap::new();
    let mut out: Vec<Vec<Vertex>> = Vec::new();
    let mut path: Vec<Vertex> = Vec::new();
    for v in body {
        match seen.get(&v.node_idx) {
            Some(&at) => {
                let mut loop_ring = path.split_off(at);
                for w in &loop_ring[1..] {
                    seen.remove(&w.node_idx);
                }
                let anchor = loop_ring[0];
                path.push(anchor);
                loop_ring.push(anchor); // close it explicitly
                if loop_ring.len() >= 4 {
                    out.push(loop_ring);
                }
            }
            None => {
                seen.insert(v.node_idx, path.len());
                path.push(*v);
            }
        }
    }

    if path.len() >= 3 {
        if explicitly_closed {
            let first = path[0];
            path.push(first);
        }
        out.push(path);
    }
    out
}

pub fn assemble_rings(segments: Vec<Vec<Vertex>>) -> Vec<Vec<Vertex>> {
    assemble_rings_and_open_chains(segments).rings
}

/// Ray-casting point-in-polygon test against a ring of `(x, y)` vertices.
fn point_in_ring(pt: (f64, f64), ring: &[(f64, f64)]) -> bool {
    let (px, py) = pt;
    let mut inside = false;
    let n = ring.len();
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = ring[i];
        let (xj, yj) = ring[j];
        if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Assemble a `type=multipolygon`/`boundary` relation into polygons, each an
/// exterior ring followed by the holes it contains, plus any unclosed
/// exterior chains. `polygons[p][r]` is ring `r` of polygon `p` (ring 0 is
/// the exterior); each vertex carries both its parent node index and its
/// resolved `(lon, lat)`.
pub fn assemble_multipolygon(
    archive: &Osm,
    rel_idx: usize,
    scale: f64,
) -> (Vec<Vec<Vec<Vertex>>>, Vec<Vec<Vertex>>) {
    let members = archive.relation_members();
    let ways = archive.ways();
    let strings = archive.stringtable();
    let nodes = archive.nodes();

    let to_vertices = |seg: &[u64]| -> Vec<Vertex> {
        seg.iter()
            .map(|&n| {
                let node = &nodes[n as usize];
                Vertex {
                    node_idx: n,
                    lon: node.lon() as f64 / scale,
                    lat: node.lat() as f64 / scale,
                }
            })
            .collect()
    };

    let mut outer_segs: Vec<Vec<Vertex>> = Vec::new();
    let mut inner_segs: Vec<Vec<Vertex>> = Vec::new();
    for member in members.at(rel_idx) {
        let RelationMembersRef::WayMember(wm) = member else {
            continue;
        };
        let Some(way_idx) = wm.way_idx() else {
            continue;
        };
        let seg = way_node_indices(archive, &ways[way_idx as usize]);
        if seg.len() < 2 {
            continue;
        }
        // Role "inner" carves holes; everything else (outer, empty) is exterior.
        if strings.substring_raw(wm.role_idx() as usize) == b"inner" {
            inner_segs.push(to_vertices(&seg));
        } else {
            outer_segs.push(to_vertices(&seg));
        }
    }

    let assembled_outers = assemble_rings_and_open_chains(outer_segs);
    let outers = assembled_outers.rings;
    let inner_rings = assemble_rings(inner_segs);

    if outers.is_empty() {
        return (Vec::new(), assembled_outers.open_chains);
    }

    // One polygon per exterior ring; assign each hole to the exterior that
    // contains its first vertex.
    let outers_xy: Vec<Vec<(f64, f64)>> = outers
        .iter()
        .map(|o| o.iter().map(|v| v.xy()).collect())
        .collect();
    let mut polygons: Vec<Vec<Vec<Vertex>>> = outers.iter().cloned().map(|o| vec![o]).collect();
    for inner in &inner_rings {
        let Some(first) = inner.first() else {
            continue;
        };
        if let Some(oi) = outers_xy.iter().position(|o| point_in_ring(first.xy(), o)) {
            polygons[oi].push(inner.clone());
        }
        // A hole with no containing exterior is dropped (malformed relation).
    }

    (polygons, assembled_outers.open_chains)
}

/// Slice `postings` by the `@range` stored at `ranges[idx]`. The generated
/// `Range::post()` reads the *next* element's `first_idx`, so a trailing
/// sentinel must close the last real range (mirrors `backrefs::slice_range`).
#[inline]
fn slice_range<'a, T>(ranges: &'a [crate::Range], postings: &'a [T], idx: usize) -> &'a [T] {
    let Some(entry) = ranges.get(idx) else {
        return &[];
    };
    let r = entry.post();
    postings
        .get(r.start as usize..r.end as usize)
        .unwrap_or(&[])
}

/// Query API over a precomputed `Multipolygons` sub-archive: reads the
/// assembled rings [`build_multipolygons`](../../osmflat-extc) wrote, with no
/// live re-stitching. `None`/empty results mean "not precomputed for this
/// relation" (not an area relation, or its outer ways didn't close at build
/// time) -- callers wanting the open leftover chains for such relations still
/// need [`assemble_multipolygon`] directly.
#[derive(Clone, Copy)]
pub struct MultipolygonsQuery<'a> {
    parent: &'a Osm,
    multipolygons: &'a crate::Multipolygons,
}

impl<'a> MultipolygonsQuery<'a> {
    /// Wrap a parent archive and its `Multipolygons` sub-archive. Prefer
    /// [`crate::ExtArchive::multipolygons`], which verifies the fingerprint
    /// first.
    #[inline]
    pub fn new(parent: &'a Osm, multipolygons: &'a crate::Multipolygons) -> Self {
        Self {
            parent,
            multipolygons,
        }
    }

    /// Precomputed polygons for the relation at `rel_idx`: `polygons[p][r]`
    /// is ring `r` of polygon `p` (ring 0 is the exterior) as resolved
    /// `(lon, lat)` vertices, resolved from the parent's own `nodes` vector.
    /// Empty when this relation has no precomputed polygons (not an area
    /// relation, not built into this sidecar, or failed to close).
    pub fn polygons(&self, rel_idx: usize) -> Vec<Vec<Vec<(f64, f64)>>> {
        let scale = self.parent.header().coord_scale() as f64;
        let nodes = self.parent.nodes();
        let resolve = |r: &Ref| -> (f64, f64) {
            let n = &nodes[r.value() as usize];
            (n.lon() as f64 / scale, n.lat() as f64 / scale)
        };

        // Each level's `Range` is an absolute index range into the *next*
        // level's flat, relation-order vector (that's how the builder wrote
        // them: one flat CSR chain per level, not reset per relation) -- so
        // indices recovered from one level's `.post()` are exactly the
        // indices to `.get()` at the next level.
        let Some(rel_entry) = self.multipolygons.rel_polygon_range().get(rel_idx) else {
            return Vec::new();
        };
        let poly_range = rel_entry.post();
        let polygon_ring_range = self.multipolygons.polygon_ring_range();
        let ring_node_range = self.multipolygons.ring_node_range();
        let node_refs = self.multipolygons.nodes();

        (poly_range.start as usize..poly_range.end as usize)
            .map(|poly_idx| {
                let Some(poly_entry) = polygon_ring_range.get(poly_idx) else {
                    return Vec::new();
                };
                let ring_range = poly_entry.post();
                (ring_range.start as usize..ring_range.end as usize)
                    .map(|ring_idx| {
                        slice_range(ring_node_range, node_refs, ring_idx)
                            .iter()
                            .map(resolve)
                            .collect()
                    })
                    .collect()
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Terser test-only vertex constructor; `node_idx` is arbitrary in these
    /// tests since only the geometric stitching is under test.
    fn v(node_idx: u64, lon: f64, lat: f64) -> Vertex {
        Vertex { node_idx, lon, lat }
    }

    #[test]
    fn ring_assembler_returns_unclosed_chain() {
        let segments = vec![
            vec![v(1, 1.0, 0.0), v(2, 2.0, 0.0)],
            vec![v(0, 0.0, 0.0), v(1, 1.0, 0.0)],
        ];

        let assembled = assemble_rings_and_open_chains(segments);

        assert!(assembled.rings.is_empty());
        assert_eq!(
            assembled.open_chains,
            vec![vec![v(0, 0.0, 0.0), v(1, 1.0, 0.0), v(2, 2.0, 0.0)]]
        );
    }

    #[test]
    fn ring_assembler_keeps_closed_ring_out_of_open_chains() {
        let segments = vec![vec![
            v(0, 0.0, 0.0),
            v(1, 0.0, 1.0),
            v(2, 1.0, 0.0),
            v(0, 0.0, 0.0),
        ]];

        let assembled = assemble_rings_and_open_chains(segments);

        assert_eq!(assembled.rings.len(), 1);
        assert!(assembled.open_chains.is_empty());
    }

    /// Reproduces the Lake Michigan multipolygon bug: when two distinct
    /// rings pass near each other (e.g. the main shoreline ring and a
    /// separate small ring around a harbor/river mouth), a decoy segment
    /// from the *other* ring can be within `RING_SNAP_TOLERANCE_M` of the
    /// open end being stitched. The assembler must splice in the *nearest*
    /// matching segment, not merely the first one it happens to encounter in
    /// list order — otherwise it stitches the wrong ring in, producing a
    /// self-intersecting polygon with a long spurious "shortcut" edge
    /// instead of the correct closed ring.
    #[test]
    fn ring_assembler_picks_nearest_endpoint_not_first_in_list() {
        // Decoy segment belonging to an unrelated ring, placed first in the
        // segment list. Its start point is within RING_SNAP_TOLERANCE_M of
        // the open ring's end (~223m), but farther away than the true
        // continuation below.
        let seg_wrong = vec![v(10, 10.002, 0.0), v(11, 20.0, 0.0)];
        // True continuation of the ring being assembled: closer to the open
        // end (~111m) than seg_wrong, but placed after it in the list.
        let seg_correct = vec![v(20, 10.001, 0.0), v(21, 10.0, 1.0), v(0, 0.0, 0.0)];
        let seg_start = vec![v(0, 0.0, 0.0), v(30, 10.0, 0.0)];

        let segments = vec![seg_wrong, seg_correct, seg_start];

        let assembled = assemble_rings_and_open_chains(segments);

        assert_eq!(
            assembled.rings,
            vec![vec![
                v(0, 0.0, 0.0),
                v(30, 10.0, 0.0),
                v(21, 10.0, 1.0),
                v(0, 0.0, 0.0)
            ]],
            "assembler should splice in the nearer segment (seg_correct), not \
             the first-in-list-order segment (seg_wrong), to produce a closed \
             ring"
        );
        assert_eq!(
            assembled.open_chains.len(),
            1,
            "the decoy segment should be left over as its own open chain"
        );
    }

    /// Reproduces the Baarle-Hertog bug: two exclaves of one boundary
    /// relation meeting at a single shared node. Stitching walks straight
    /// through that junction (both "continue" and "close" are locally valid
    /// there), yielding one figure-eight ring; the assembler has to split it
    /// back into the two rings osmium's area assembler reports, or a renderer
    /// draws one polygon too few.
    #[test]
    fn ring_assembler_splits_ring_that_touches_itself_at_a_node() {
        // Big square and small square sharing exactly node 0 at the corner,
        // presented as one already-closed traversal of both.
        let figure_eight = vec![
            v(0, 0.0, 0.0),
            v(1, 0.0, 1.0),
            v(2, 1.0, 1.0),
            v(3, 1.0, 0.0),
            v(0, 0.0, 0.0),
            v(4, 0.0, -0.5),
            v(5, -0.5, -0.5),
            v(6, -0.5, 0.0),
            v(0, 0.0, 0.0),
        ];

        let assembled = assemble_rings_and_open_chains(vec![figure_eight]);

        assert_eq!(
            assembled.rings.len(),
            2,
            "a ring passing through the same node twice is two rings, not one"
        );
        assert!(assembled.open_chains.is_empty());
        for ring in &assembled.rings {
            assert_eq!(
                ring.first().unwrap().node_idx,
                ring.last().unwrap().node_idx,
                "each split piece must still be explicitly closed"
            );
        }
        let sizes: Vec<usize> = assembled.rings.iter().map(|r| r.len()).collect();
        assert_eq!(sizes, vec![5, 5]);
    }

    /// Reproduces the Elliott Bay bug: once a ring has one stitch behind it,
    /// the assembler treated *any* wander back within `RING_SNAP_TOLERANCE_M`
    /// of the ring's start as "done", even when a segment was still available
    /// that plainly continues the same ring further. A jagged real-world
    /// shoreline can pass back near its own starting point long before it's
    /// actually finished; the old code closed right there and threw away the
    /// rest, leaving a straight spurious "shortcut" edge across the shape
    /// instead of the correct, larger closed ring.
    #[test]
    fn ring_assembler_prefers_continuing_over_premature_close() {
        // Ring start.
        let seg_start = vec![v(0, 0.0, 0.0), v(1, 10.0, 0.0)];
        // Wanders back to ~111m from (0.0, 0.0) -- within RING_SNAP_TOLERANCE_M
        // -- but this is *not* actually the closing seam; the ring is meant to
        // continue on to the far loop below and close there instead.
        let seg_near_start = vec![v(1, 10.0, 0.0), v(2, 0.001, 0.0)];
        // The true continuation: a large loop that closes the ring exactly
        // back at (0.0, 0.0).
        let seg_far_loop = vec![v(2, 0.001, 0.0), v(3, 5.0, 5.0), v(0, 0.0, 0.0)];

        // `Vec::pop()` takes from the *end*, and the ring being assembled
        // always starts from whatever's popped first -- order here so
        // `seg_start` is popped first, exactly like the real relation where
        // the ring-in-progress is the one that wanders back near its own
        // start, not one of the not-yet-touched remaining segments.
        let segments = vec![seg_far_loop, seg_near_start, seg_start];

        let assembled = assemble_rings_and_open_chains(segments);

        assert_eq!(
            assembled.rings,
            vec![vec![
                v(0, 0.0, 0.0),
                v(1, 10.0, 0.0),
                v(2, 0.001, 0.0),
                v(3, 5.0, 5.0),
                v(0, 0.0, 0.0),
            ]],
            "assembler should keep stitching through the near-start wander and \
             close the full, larger ring, instead of stopping early at the \
             coincidental near-start point"
        );
        assert!(assembled.open_chains.is_empty());
    }
}
