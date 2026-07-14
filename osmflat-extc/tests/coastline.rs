#![cfg(feature = "test-support")]

//! End-to-end check for `--coastline`: build synthetic `natural=coastline`
//! ways -- a big CCW "continent" ring and a small CW "inland sea" ring, each
//! split across two ways so real stitching happens -- and confirm the
//! precomputed sidecar's rings match live `assemble_coastline` on the same
//! parent archive: correct count, correct land/water classification, sorted
//! by area descending, and geometry that round-trips through the sidecar
//! (stored as parent node indices) back to the original coordinates.

use osmflat_ext::coastline::assemble_coastline;
use osmflat_extc::test_support::{
    build_ext_archive, build_parent_archive, Fixture, NodeSpec, TagSpec, WaySpec, COORD_SCALE,
};
use osmflat_extc::BuildOptions;

fn tag(key: &'static str, value: &'static str) -> TagSpec {
    TagSpec::new(key, value)
}

fn node(lon: f64, lat: f64) -> NodeSpec {
    NodeSpec {
        lon,
        lat,
        tags: vec![],
    }
}

/// A big CCW "continent" square (0,0)-(10,10) and a small CW "inland sea"
/// square (2,2)-(3,3), each split across two `natural=coastline` ways that
/// share endpoints -- real stitching, not single self-closed ways. Neither
/// ring is a relation member; coastline is collected from plain ways.
fn fixture() -> Fixture {
    Fixture {
        nodes: vec![
            // Big ring corners: 0..3 (4th closes back to node 0).
            node(0.0, 0.0),
            node(10.0, 0.0),
            node(10.0, 10.0),
            node(0.0, 10.0),
            // Small ring corners: 4..7 (4th closes back to node 4).
            node(2.0, 2.0),
            node(2.0, 3.0),
            node(3.0, 3.0),
            node(3.0, 2.0),
        ],
        ways: vec![
            // Big ring, CCW, split into two ways sharing nodes 0 and 2.
            WaySpec {
                refs: vec![0, 1, 2],
                tags: vec![tag("natural", "coastline")],
            },
            WaySpec {
                refs: vec![2, 3, 0],
                tags: vec![tag("natural", "coastline")],
            },
            // Small ring, CW (reversed relative to the big ring's winding),
            // split into two ways sharing nodes 4 and 6.
            WaySpec {
                refs: vec![4, 5, 6],
                tags: vec![tag("natural", "coastline")],
            },
            WaySpec {
                refs: vec![6, 7, 4],
                tags: vec![tag("natural", "coastline")],
            },
        ],
        relations: vec![],
    }
}

#[test]
fn precomputed_coastline_matches_live_assembly() {
    let parent = build_parent_archive(&fixture()).unwrap();

    // Live assembly, straight off the parent -- the ground truth.
    let live = assemble_coastline(&parent, COORD_SCALE as f64);
    assert_eq!(live.len(), 2, "two independent closed rings");
    assert!(
        live[0].area_m2 > live[1].area_m2,
        "sorted by area descending"
    );
    assert!(live[0].is_land, "big CCW ring is land");
    assert!(
        !live[1].is_land,
        "small CW ring is water enclosed by coastline"
    );

    // Precomputed via the sidecar -- must match exactly (count, order,
    // classification, and geometry resolved back from stored node indices).
    let opts = BuildOptions {
        coastline: true,
        ..Default::default()
    };
    let ext_archive = build_ext_archive(parent, &opts).unwrap();
    let coastline = ext_archive.coastline().expect("built with --coastline");
    let precomputed = coastline.rings();

    assert_eq!(precomputed.len(), live.len());
    for (p, l) in precomputed.iter().zip(live.iter()) {
        assert_eq!(p.is_land, l.is_land);
        let p_coords: Vec<(f64, f64)> = p.vertices.clone();
        let l_coords: Vec<(f64, f64)> = l.vertices.iter().map(|v| (v.lon, v.lat)).collect();
        assert_eq!(p_coords, l_coords);
    }
}
