#![cfg(feature = "test-support")]

//! End-to-end check for `--multipolygons`: build a synthetic `type=multipolygon`
//! relation whose outer ring is split across two ways (so real stitching
//! happens, not just a single already-closed way), then confirm the
//! precomputed sidecar's `polygons()` matches what live `assemble_multipolygon`
//! produces directly against the same parent archive.

use osmflat::find_tag;
use osmflat_ext::multipolygon::assemble_multipolygon;
use osmflat_extc::test_support::{
    build_ext_archive, build_parent_archive, Fixture, MemberSpec, NodeSpec, RelationSpec, TagSpec,
    WaySpec, COORD_SCALE,
};
use osmflat_extc::BuildOptions;

fn tag(key: &'static str, value: &'static str) -> TagSpec {
    TagSpec::new(key, value)
}

/// A unit-square multipolygon relation, its outer ring split across two ways
/// that share endpoints (0,0) and (1,1) -- real stitching, not a single
/// self-closed way.
fn square_fixture() -> Fixture {
    Fixture {
        nodes: vec![
            NodeSpec {
                lon: 0.0,
                lat: 0.0,
                tags: vec![],
            },
            NodeSpec {
                lon: 0.0,
                lat: 1.0,
                tags: vec![],
            },
            NodeSpec {
                lon: 1.0,
                lat: 1.0,
                tags: vec![],
            },
            NodeSpec {
                lon: 1.0,
                lat: 0.0,
                tags: vec![],
            },
        ],
        ways: vec![
            WaySpec {
                refs: vec![0, 1, 2],
                tags: vec![],
            },
            WaySpec {
                refs: vec![2, 3, 0],
                tags: vec![],
            },
        ],
        relations: vec![RelationSpec {
            bbox: Some((0.0, 0.0, 1.0, 1.0)),
            members: vec![MemberSpec::Way(0), MemberSpec::Way(1)],
            tags: vec![tag("type", "multipolygon"), tag("natural", "water")],
        }],
    }
}

#[test]
fn precomputed_polygons_match_live_assembly() {
    let parent = build_parent_archive(&square_fixture()).unwrap();

    let rel_idx = (0..parent.relations().len())
        .find(|&i| find_tag(&parent, parent.relations()[i].tags(), b"natural") == Some(b"water"))
        .expect("fixture relation must survive spatial reordering");

    // Live assembly, straight off the parent -- the ground truth.
    let (live_polygons, live_open_chains) =
        assemble_multipolygon(&parent, rel_idx, COORD_SCALE as f64);
    assert_eq!(live_polygons.len(), 1, "one exterior ring, no holes");
    assert!(live_open_chains.is_empty(), "the ring should fully close");
    let live_coords: Vec<Vec<(f64, f64)>> = live_polygons[0]
        .iter()
        .map(|ring| ring.iter().map(|v| (v.lon, v.lat)).collect())
        .collect();

    // Precomputed via the sidecar -- must match exactly.
    let opts = BuildOptions {
        multipolygons: true,
        ..Default::default()
    };
    let ext_archive = build_ext_archive(parent, &opts).unwrap();
    let precomputed = ext_archive
        .multipolygons()
        .expect("built with --multipolygons")
        .polygons(rel_idx);

    assert_eq!(
        precomputed,
        vec![live_coords],
        "precomputed sidecar polygons must match live assembly exactly"
    );
}

#[test]
fn non_area_relation_has_no_precomputed_polygons() {
    let mut fixture = square_fixture();
    // Same shape, but not an area relation (no type=multipolygon/boundary).
    fixture.relations[0].tags = vec![tag("type", "route"), tag("route", "bicycle")];

    let parent = build_parent_archive(&fixture).unwrap();
    let rel_idx = (0..parent.relations().len())
        .find(|&i| find_tag(&parent, parent.relations()[i].tags(), b"route") == Some(b"bicycle"))
        .unwrap();

    let opts = BuildOptions {
        multipolygons: true,
        ..Default::default()
    };
    let ext_archive = build_ext_archive(parent, &opts).unwrap();
    let precomputed = ext_archive.multipolygons().unwrap().polygons(rel_idx);

    assert!(precomputed.is_empty());
}
