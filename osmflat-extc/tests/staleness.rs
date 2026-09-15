#![cfg(feature = "test-support")]

//! The fingerprint guard: a sidecar built for one parent must refuse to open
//! against any other parent build (design §2, §9 "Staleness").

use osmflat_ext::fingerprint::Mismatch;
use osmflat_ext::ExtArchive;
use osmflat_extc::test_support::{
    build_ext, build_parent_archive, Fixture, MemberSpec, NodeSpec, RelationSpec, TagSpec, WaySpec,
};

fn tag(key: &'static str, value: &'static str) -> TagSpec {
    TagSpec::new(key, value)
}

fn fixture() -> Fixture {
    Fixture {
        nodes: vec![
            NodeSpec {
                lon: -77.03,
                lat: 38.90,
                tags: vec![tag("amenity", "cafe"), tag("name", "Alpha")],
            },
            NodeSpec {
                lon: -77.02,
                lat: 38.91,
                tags: vec![tag("highway", "crossing")],
            },
        ],
        ways: vec![WaySpec {
            refs: vec![0, 1],
            tags: vec![tag("highway", "footway")],
        }],
        relations: vec![RelationSpec {
            bbox: Some((-77.03, 38.90, -77.02, 38.91)),
            members: vec![MemberSpec::Way(0)],
            tags: vec![tag("type", "route")],
        }],
    }
}

fn ext_for(fixture: &Fixture) -> osmflat_ext::Ext {
    let parent = build_parent_archive(fixture).expect("build parent");
    build_ext(
        &parent,
        &osmflat_extc::BuildOptions {
            taginfo: true,
            backrefs: true,
            ..Default::default()
        },
    )
    .expect("build sidecar")
}

/// Open `ext` against a parent rebuilt from `rebuilt`.
fn open_against(ext: osmflat_ext::Ext, rebuilt: &Fixture) -> Result<ExtArchive, Mismatch> {
    ExtArchive::open(
        build_parent_archive(rebuilt).expect("build rebuilt parent"),
        ext,
    )
}

fn count_mismatch(result: Result<ExtArchive, Mismatch>) -> (&'static str, u64, u64) {
    match result {
        Err(Mismatch::Count {
            what,
            expected,
            actual,
        }) => (what, expected, actual),
        Err(other) => panic!("expected a count mismatch, got {other:?}"),
        Ok(_) => panic!("stale sidecar opened against a different parent"),
    }
}

#[test]
fn sidecar_opens_against_an_identical_rebuild() {
    let ext = ext_for(&fixture());
    let archive = open_against(ext, &fixture()).expect("identical rebuild must match");
    assert!(archive.taginfo().is_some());
    assert!(archive.backrefs().is_some());
}

#[test]
fn sidecar_refuses_parent_with_an_extra_node() {
    let mut rebuilt = fixture();
    rebuilt.nodes.push(NodeSpec {
        lon: -77.01,
        lat: 38.92,
        tags: vec![],
    });
    // `expected` is the live parent, `actual` what the sidecar recorded.
    assert_eq!(
        count_mismatch(open_against(ext_for(&fixture()), &rebuilt)),
        ("nodes", 3, 2)
    );
}

#[test]
fn sidecar_refuses_parent_with_an_extra_way() {
    let mut rebuilt = fixture();
    rebuilt.ways.push(WaySpec {
        refs: vec![1, 0],
        tags: vec![],
    });
    assert_eq!(
        count_mismatch(open_against(ext_for(&fixture()), &rebuilt)),
        ("ways", 2, 1)
    );
}

#[test]
fn sidecar_refuses_parent_with_an_extra_relation() {
    let mut rebuilt = fixture();
    rebuilt.relations.push(RelationSpec {
        bbox: None,
        members: vec![MemberSpec::Node(0)],
        tags: vec![],
    });
    assert_eq!(
        count_mismatch(open_against(ext_for(&fixture()), &rebuilt)),
        ("relations", 2, 1)
    );
}

#[test]
fn sidecar_refuses_parent_with_a_new_distinct_tag() {
    let mut rebuilt = fixture();
    rebuilt.nodes[1].tags.push(tag("crossing", "marked"));
    let (what, expected, actual) = count_mismatch(open_against(ext_for(&fixture()), &rebuilt));
    assert_eq!(what, "tags");
    assert_eq!(expected, actual + 1);
}

#[test]
fn sidecar_refuses_parent_whose_strings_changed_but_counts_did_not() {
    // Same entities and tags, only a longer string: every vector length is
    // unchanged, so the stringtable length is what catches the rebuild.
    let mut rebuilt = fixture();
    rebuilt.nodes[0].tags[1] = tag("name", "Alpha Bravo");
    let (what, expected, actual) = count_mismatch(open_against(ext_for(&fixture()), &rebuilt));
    assert_eq!(what, "stringtable");
    assert_eq!(expected, actual + " Bravo".len() as u64);
}
