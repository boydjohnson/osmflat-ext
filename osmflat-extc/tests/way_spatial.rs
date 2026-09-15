#![cfg(feature = "test-support")]

//! Way and relation spatial queries ("any part touches") against a brute-force
//! oracle that reads geometry straight from the parent, with no bbox prefilter
//! and a different distance formula (clamped projection) than the library.

use osmflat::{Osm, RelationMembersRef};
use osmflat_ext::spatial::{
    k_nearest_relations, k_nearest_ways, relations_in_polygon, relations_within_radius,
    ways_in_polygon, ways_within_radius, Point,
};
use osmflat_extc::test_support::{
    build_parent_archive, Fixture, MemberSpec, NodeSpec, RelationSpec, WaySpec, COORD_SCALE,
};
use std::collections::HashSet;

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() * n as f64) as usize
    }
}

const RELATIONS: usize = 30;

/// 300 nodes, 80 ways (some single-node), 30 relations mixing node, way, and
/// relation members, including a self-reference and a two-relation cycle.
/// Relation bboxes cover their members' geometry, as osmflatc computes them.
fn fixture() -> Fixture {
    let mut rng = Lcg(0xfeed_beef_0bad_cafe);
    let nodes: Vec<NodeSpec> = (0..300)
        .map(|_| NodeSpec {
            lon: -77.05 + rng.next() * 0.1,
            lat: 38.85 + rng.next() * 0.1,
            tags: vec![],
        })
        .collect();
    let ways: Vec<WaySpec> = (0..80)
        .map(|w| {
            // Every 10th way is a single node; the rest are short linestrings
            // whose later vertices stay near the first, so segments are local.
            let len = if w % 10 == 0 { 1 } else { 2 + rng.below(4) };
            let refs = (0..len).map(|_| rng.below(nodes.len())).collect();
            WaySpec { refs, tags: vec![] }
        })
        .collect();

    let mut members: Vec<Vec<MemberSpec>> = (0..RELATIONS)
        .map(|r| {
            let mut m = Vec::new();
            for _ in 0..1 + rng.below(3) {
                m.push(match rng.below(3) {
                    0 => MemberSpec::Node(rng.below(nodes.len())),
                    1 => MemberSpec::Way(rng.below(ways.len())),
                    _ if r > 0 => MemberSpec::Relation(rng.below(r)),
                    _ => MemberSpec::Way(rng.below(ways.len())),
                });
            }
            m
        })
        .collect();
    // A relation containing only relations, a self-reference, and a cycle.
    members[25] = vec![MemberSpec::Relation(3), MemberSpec::Relation(7)];
    members[26].push(MemberSpec::Relation(26));
    members[28].push(MemberSpec::Relation(29));
    members[29].push(MemberSpec::Relation(28));

    // Relation bboxes: fixpoint over members (relations can nest and cycle).
    let point_bbox = |n: usize| (nodes[n].lon, nodes[n].lat, nodes[n].lon, nodes[n].lat);
    let union = |a: Option<(f64, f64, f64, f64)>, b: (f64, f64, f64, f64)| {
        Some(match a {
            None => b,
            Some(a) => (a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3)),
        })
    };
    let mut bboxes: Vec<Option<(f64, f64, f64, f64)>> = vec![None; RELATIONS];
    loop {
        let mut changed = false;
        for r in 0..RELATIONS {
            let mut bbox = None;
            for m in &members[r] {
                match *m {
                    MemberSpec::Node(n) => bbox = union(bbox, point_bbox(n)),
                    MemberSpec::Way(w) => {
                        for &n in &ways[w].refs {
                            bbox = union(bbox, point_bbox(n));
                        }
                    }
                    MemberSpec::Relation(c) => {
                        if let Some(b) = bboxes[c] {
                            bbox = union(bbox, b);
                        }
                    }
                }
            }
            if bbox != bboxes[r] {
                bboxes[r] = bbox;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let relations = members
        .into_iter()
        .zip(bboxes)
        .map(|(members, bbox)| RelationSpec {
            bbox,
            members,
            tags: vec![],
        })
        .collect();
    Fixture {
        nodes,
        ways,
        relations,
    }
}

// ---------------------------------------------------------------------------
// Oracle: f64 geometry in archive units, straight from the parent archive.
// ---------------------------------------------------------------------------

type P = (f64, f64);

fn scaled(lon: f64, lat: f64) -> P {
    let s = COORD_SCALE as f64;
    ((lon * s) as i32 as f64, (lat * s) as i32 as f64)
}

fn way_points(parent: &Osm, w: usize) -> Vec<P> {
    let refs = parent.ways()[w].refs();
    let nodes_index = parent.nodes_index();
    (refs.start..refs.end)
        .filter_map(|i| nodes_index[i as usize].value())
        .map(|n| {
            let node = &parent.nodes()[n as usize];
            (node.lon() as f64, node.lat() as f64)
        })
        .collect()
}

/// Squared distance from `p` to segment `ab` by clamped projection.
fn seg_dist_sq(p: P, a: P, b: P) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len_sq = dx * dx + dy * dy;
    let t = if len_sq == 0.0 {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len_sq).clamp(0.0, 1.0)
    };
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    (p.0 - cx).powi(2) + (p.1 - cy).powi(2)
}

fn way_dist_sq(parent: &Osm, w: usize, c: P) -> Option<f64> {
    let pts = way_points(parent, w);
    match pts.len() {
        0 => None,
        1 => Some(seg_dist_sq(c, pts[0], pts[0])),
        _ => pts
            .windows(2)
            .map(|s| seg_dist_sq(c, s[0], s[1]))
            .reduce(f64::min),
    }
}

fn cross(o: P, a: P, b: P) -> f64 {
    (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0)
}

fn on_seg(p: P, a: P, b: P) -> bool {
    cross(a, b, p) == 0.0
        && p.0 >= a.0.min(b.0)
        && p.0 <= a.0.max(b.0)
        && p.1 >= a.1.min(b.1)
        && p.1 <= a.1.max(b.1)
}

fn segs_cross(p1: P, p2: P, q1: P, q2: P) -> bool {
    let (d1, d2) = (cross(q1, q2, p1), cross(q1, q2, p2));
    let (d3, d4) = (cross(p1, p2, q1), cross(p1, p2, q2));
    (d1 * d2 < 0.0 && d3 * d4 < 0.0)
        || on_seg(p1, q1, q2)
        || on_seg(p2, q1, q2)
        || on_seg(q1, p1, p2)
        || on_seg(q2, p1, p2)
}

fn inside(p: P, poly: &[P]) -> bool {
    let n = poly.len();
    if (0..n).any(|i| on_seg(p, poly[i], poly[(i + 1) % n])) {
        return true;
    }
    let mut c = false;
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            c = !c;
        }
    }
    c
}

fn way_touches_poly(parent: &Osm, w: usize, poly: &[P]) -> bool {
    let pts = way_points(parent, w);
    pts.iter().any(|&p| inside(p, poly))
        || pts.windows(2).any(|s| {
            (0..poly.len()).any(|i| segs_cross(s[0], s[1], poly[i], poly[(i + 1) % poly.len()]))
        })
}

/// Every node point and way reachable from relation `r` (each relation once).
fn relation_parts(parent: &Osm, r: usize) -> (Vec<P>, Vec<usize>) {
    let (mut points, mut ways) = (Vec::new(), Vec::new());
    let mut seen = HashSet::from([r]);
    let mut stack = vec![r];
    while let Some(r) = stack.pop() {
        for m in parent.relation_members().at(r) {
            match m {
                RelationMembersRef::NodeMember(m) => {
                    if let Some(n) = m.node_idx() {
                        let node = &parent.nodes()[n as usize];
                        points.push((node.lon() as f64, node.lat() as f64));
                    }
                }
                RelationMembersRef::WayMember(m) => ways.extend(m.way_idx().map(|w| w as usize)),
                RelationMembersRef::RelationMember(m) => {
                    if let Some(c) = m.relation_idx() {
                        if seen.insert(c as usize) {
                            stack.push(c as usize);
                        }
                    }
                }
            }
        }
    }
    (points, ways)
}

fn relation_dist_sq(parent: &Osm, r: usize, c: P) -> Option<f64> {
    let (points, ways) = relation_parts(parent, r);
    points
        .iter()
        .map(|&p| seg_dist_sq(c, p, p))
        .chain(ways.iter().filter_map(|&w| way_dist_sq(parent, w, c)))
        .reduce(f64::min)
}

fn relation_touches_poly(parent: &Osm, r: usize, poly: &[P]) -> bool {
    let (points, ways) = relation_parts(parent, r);
    points.iter().any(|&p| inside(p, poly))
        || ways.iter().any(|&w| way_touches_poly(parent, w, poly))
}

/// Indices whose distance is within `radius` degrees, nearest-first.
fn within_oracle(dist: impl Fn(usize) -> Option<f64>, n: usize, radius: f64) -> Vec<usize> {
    let r = (radius * COORD_SCALE as f64).ceil();
    let mut hits: Vec<(f64, usize)> = (0..n)
        .filter_map(|i| dist(i).filter(|&d| d <= r * r).map(|d| (d, i)))
        .collect();
    hits.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    hits.into_iter().map(|(_, i)| i).collect()
}

fn nearest_oracle(dist: impl Fn(usize) -> Option<f64>, n: usize, k: usize) -> Vec<usize> {
    let mut all: Vec<(f64, usize)> = (0..n).filter_map(|i| dist(i).map(|d| (d, i))).collect();
    all.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    all.into_iter().take(k).map(|(_, i)| i).collect()
}

fn centers() -> Vec<(f64, f64)> {
    let mut rng = Lcg(42);
    let mut c: Vec<(f64, f64)> = (0..12)
        .map(|_| (-77.05 + rng.next() * 0.1, 38.85 + rng.next() * 0.1))
        .collect();
    // Outside the data, far away, and at a world corner.
    c.extend([(-77.2, 38.9), (10.0, -45.0), (180.0, 90.0)]);
    c
}

#[test]
fn ways_and_relations_within_radius_match_oracle() {
    let parent = build_parent_archive(&fixture()).expect("build parent");
    let (n_ways, n_rels) = (parent.ways().len(), parent.relations().len());
    let mut hits = 0;
    for (lon, lat) in centers() {
        let c = scaled(lon, lat);
        for radius in [0.0, 0.002, 0.01, 0.03, 0.2] {
            let got: Vec<usize> = ways_within_radius(&parent, lon, lat, radius).collect();
            let want = within_oracle(|w| way_dist_sq(&parent, w, c), n_ways, radius);
            assert_eq!(got, want, "ways within {radius} of ({lon}, {lat})");
            hits += got.len();

            let got: Vec<usize> = relations_within_radius(&parent, lon, lat, radius).collect();
            let want = within_oracle(|r| relation_dist_sq(&parent, r, c), n_rels, radius);
            assert_eq!(got, want, "relations within {radius} of ({lon}, {lat})");
            hits += got.len();
        }
    }
    assert!(hits > 500, "only {hits} hits");
}

#[test]
fn ways_and_relations_in_polygon_match_oracle() {
    let parent = build_parent_archive(&fixture()).expect("build parent");
    let p = |lon, lat| Point { lon, lat };
    let polygons = vec![
        vec![p(-77.04, 38.86), p(-76.96, 38.87), p(-77.0, 38.94)],
        // Concave "L".
        vec![
            p(-77.04, 38.86),
            p(-77.0, 38.86),
            p(-77.0, 38.89),
            p(-76.97, 38.89),
            p(-76.97, 38.93),
            p(-77.04, 38.93),
        ],
        // A thin sliver that long segments cross without a vertex inside.
        vec![
            p(-77.001, 38.85),
            p(-76.999, 38.85),
            p(-76.999, 38.95),
            p(-77.001, 38.95),
        ],
        // Tiny, and far away.
        vec![p(-77.0, 38.9), p(-76.9995, 38.9), p(-76.9995, 38.9005)],
        vec![p(10.0, 10.0), p(10.1, 10.0), p(10.1, 10.1)],
    ];
    let mut hits = 0;
    let mut crossings_only = 0;
    for poly in &polygons {
        let scaled_poly: Vec<P> = poly.iter().map(|q| scaled(q.lon, q.lat)).collect();

        let got: Vec<usize> = ways_in_polygon(&parent, poly).collect();
        let want: Vec<usize> = (0..parent.ways().len())
            .filter(|&w| way_touches_poly(&parent, w, &scaled_poly))
            .collect();
        assert_eq!(got, want, "ways in {poly:?}");
        hits += got.len();
        crossings_only += got
            .iter()
            .filter(|&&w| {
                !way_points(&parent, w)
                    .iter()
                    .any(|&q| inside(q, &scaled_poly))
            })
            .count();

        let got: Vec<usize> = relations_in_polygon(&parent, poly).collect();
        let want: Vec<usize> = (0..parent.relations().len())
            .filter(|&r| relation_touches_poly(&parent, r, &scaled_poly))
            .collect();
        assert_eq!(got, want, "relations in {poly:?}");
        hits += got.len();
    }
    assert!(hits > 60, "only {hits} hits");
    // The sliver must exercise the segment-crossing path, not just vertices.
    assert!(crossings_only > 0, "no way matched by crossing alone");
}

#[test]
fn k_nearest_ways_and_relations_match_oracle() {
    let parent = build_parent_archive(&fixture()).expect("build parent");
    let (n_ways, n_rels) = (parent.ways().len(), parent.relations().len());
    for (lon, lat) in centers() {
        let c = scaled(lon, lat);
        for k in [1, 3, 10, 40, 1000] {
            assert_eq!(
                k_nearest_ways(&parent, lon, lat, k),
                nearest_oracle(|w| way_dist_sq(&parent, w, c), n_ways, k),
                "{k} nearest ways to ({lon}, {lat})"
            );
            assert_eq!(
                k_nearest_relations(&parent, lon, lat, k),
                nearest_oracle(|r| relation_dist_sq(&parent, r, c), n_rels, k),
                "{k} nearest relations to ({lon}, {lat})"
            );
        }
    }
}
