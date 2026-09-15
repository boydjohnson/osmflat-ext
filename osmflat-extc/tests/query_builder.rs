#![cfg(feature = "test-support")]

//! `ExtArchive::query()`: combined tag + spatial queries must equal the
//! intersection of each constraint's independently computed result.

use osmflat::Osm;
use osmflat_ext::query::{self, Bbox, EntityType, QueryError};
use osmflat_ext::spatial::{nodes_in_polygon, nodes_within_radius, Point};
use osmflat_ext::ExtArchive;
use osmflat_extc::test_support::{
    build_ext_archive, build_parent_archive, Fixture, MemberSpec, NodeSpec, RelationSpec, TagSpec,
    WaySpec,
};
use std::collections::BTreeSet;

/// Deterministic LCG in `[0, 1)`, so the fixture is reproducible without a
/// dev-dependency.
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

const AMENITIES: [&str; 2] = ["cafe", "bar"];
const NAMES: [&str; 3] = ["A", "B", "C"];
const HIGHWAYS: [&str; 2] = ["residential", "primary"];

/// ~400 nodes, 60 ways, 20 relations over a 0.1° square, with overlapping
/// tags so multi-tag intersections are non-trivial.
fn fixture() -> Fixture {
    let mut rng = Lcg(0x5eed_1234_abcd_ef01);
    let mut nodes = Vec::new();
    for _ in 0..400 {
        let mut tags = Vec::new();
        if rng.next() < 0.5 {
            tags.push(TagSpec::new("amenity", AMENITIES[rng.below(2)]));
        }
        if rng.next() < 0.6 {
            tags.push(TagSpec::new("name", NAMES[rng.below(3)]));
        }
        if rng.next() < 0.2 {
            tags.push(TagSpec::new("highway", "crossing"));
        }
        nodes.push(NodeSpec {
            lon: -77.05 + rng.next() * 0.1,
            lat: 38.85 + rng.next() * 0.1,
            tags,
        });
    }

    let mut ways = Vec::new();
    for _ in 0..60 {
        let refs = (0..2 + rng.below(3))
            .map(|_| rng.below(nodes.len()))
            .collect();
        let mut tags = vec![TagSpec::new("highway", HIGHWAYS[rng.below(2)])];
        if rng.next() < 0.6 {
            tags.push(TagSpec::new("name", NAMES[rng.below(3)]));
        }
        ways.push(WaySpec { refs, tags });
    }

    let mut relations = Vec::new();
    for _ in 0..20 {
        let (lon, lat) = (-77.05 + rng.next() * 0.09, 38.85 + rng.next() * 0.09);
        let bbox = (rng.next() < 0.85).then_some((lon, lat, lon + 0.01, lat + 0.01));
        let members = (0..1 + rng.below(3))
            .map(|_| MemberSpec::Way(rng.below(ways.len())))
            .collect();
        let mut tags = vec![TagSpec::new("type", "route")];
        if rng.next() < 0.6 {
            tags.push(TagSpec::new("name", NAMES[rng.below(3)]));
        }
        relations.push(RelationSpec {
            bbox,
            members,
            tags,
        });
    }

    Fixture {
        nodes,
        ways,
        relations,
    }
}

fn archive(opts: osmflat_extc::BuildOptions) -> ExtArchive {
    let parent = build_parent_archive(&fixture()).expect("build parent");
    build_ext_archive(parent, &opts).expect("build sidecar")
}

fn taginfo_archive() -> ExtArchive {
    archive(osmflat_extc::BuildOptions {
        taginfo: true,
        ..Default::default()
    })
}

/// Entities of `entity` type carrying the exact tag `key=value`, by scanning
/// the parent (no sidecar).
fn tag_by_scan(parent: &Osm, entity: EntityType, key: &str, value: &str) -> BTreeSet<u64> {
    let tags = parent.tags();
    let tags_index = parent.tags_index();
    let strings = parent.stringtable();
    let has = |range: std::ops::Range<u64>| {
        range.into_iter().any(|ti| {
            let tag = &tags[tags_index[ti as usize].value() as usize];
            strings.substring_raw(tag.key_idx() as usize) == key.as_bytes()
                && strings.substring_raw(tag.value_idx() as usize) == value.as_bytes()
        })
    };
    let ranges: Vec<std::ops::Range<u64>> = match entity {
        EntityType::Node => parent.nodes().iter().map(|n| n.tags()).collect(),
        EntityType::Way => parent.ways().iter().map(|w| w.tags()).collect(),
        EntityType::Relation => parent.relations().iter().map(|r| r.tags()).collect(),
    };
    ranges
        .into_iter()
        .enumerate()
        .filter(|(_, r)| has(r.clone()))
        .map(|(i, _)| i as u64)
        .collect()
}

#[derive(Clone, Debug)]
enum Spatial {
    Bbox(Bbox),
    Radius(f64, f64, f64),
    Polygon(Vec<Point>),
}

impl Spatial {
    fn applies_to(&self, entity: EntityType) -> bool {
        matches!(self, Spatial::Bbox(_)) || entity == EntityType::Node
    }

    /// This constraint's own result, from the independent per-constraint API.
    fn by_itself(&self, parent: &Osm, entity: EntityType) -> BTreeSet<u64> {
        match (self, entity) {
            (Spatial::Bbox(b), EntityType::Node) => query::node_indices_in_bbox(parent, *b),
            (Spatial::Bbox(b), EntityType::Way) => query::way_indices_in_bbox(parent, *b),
            (Spatial::Bbox(b), EntityType::Relation) => query::relation_indices_in_bbox(parent, *b),
            (Spatial::Radius(lon, lat, r), _) => nodes_within_radius(parent, *lon, *lat, *r)
                .map(|i| i as u64)
                .collect(),
            (Spatial::Polygon(p), _) => nodes_in_polygon(parent, p).map(|i| i as u64).collect(),
        }
        .into_iter()
        .collect()
    }
}

fn bbox(min_lon: f64, min_lat: f64, max_lon: f64, max_lat: f64) -> Bbox {
    Bbox {
        min_lon,
        min_lat,
        max_lon,
        max_lat,
    }
}

fn point(lon: f64, lat: f64) -> Point {
    Point { lon, lat }
}

#[test]
fn query_equals_intersection_of_each_constraint() {
    let archive = taginfo_archive();
    let parent = archive.parent();

    let tag_sets: &[&[(&str, &str)]] = &[
        &[],
        &[("amenity", "cafe")],
        &[("name", "A")],
        &[("amenity", "cafe"), ("name", "B")],
        &[("amenity", "bar"), ("name", "C"), ("highway", "crossing")],
        &[("highway", "primary"), ("name", "A")],
        &[("type", "route"), ("name", "C")],
        // Present key, absent value; and an absent key.
        &[("amenity", "library")],
        &[("amenity", "cafe"), ("no_such_key", "x")],
    ];
    let triangle = vec![
        point(-77.04, 38.86),
        point(-76.96, 38.87),
        point(-77.0, 38.94),
    ];
    let spatial_sets: Vec<Vec<Spatial>> = vec![
        vec![],
        vec![Spatial::Bbox(bbox(-77.03, 38.87, -76.99, 38.92))],
        vec![
            Spatial::Bbox(bbox(-77.05, 38.85, -76.98, 38.91)),
            Spatial::Bbox(bbox(-77.02, 38.88, -76.95, 38.95)),
        ],
        vec![Spatial::Radius(-77.0, 38.9, 0.025)],
        vec![Spatial::Polygon(triangle.clone())],
        vec![
            Spatial::Bbox(bbox(-77.04, 38.86, -76.97, 38.93)),
            Spatial::Radius(-77.01, 38.9, 0.03),
            Spatial::Polygon(triangle),
        ],
        // A bbox that holds nothing.
        vec![Spatial::Bbox(bbox(10.0, 10.0, 10.1, 10.1))],
    ];

    let mut non_empty = 0;
    for entity in [EntityType::Node, EntityType::Way, EntityType::Relation] {
        for tags in tag_sets {
            for spatial in &spatial_sets {
                let mut q = archive.query();
                for (k, v) in tags.iter() {
                    q = q.with_tag(k, v);
                }
                for s in spatial {
                    q = match s {
                        Spatial::Bbox(b) => q.in_bbox(*b),
                        Spatial::Radius(lon, lat, r) => q.within_radius(*lon, *lat, *r),
                        Spatial::Polygon(p) => q.in_polygon(p),
                    };
                }
                let got = match entity {
                    EntityType::Node => q.nodes(),
                    EntityType::Way => q.ways(),
                    EntityType::Relation => q.relations(),
                };
                let ctx = format!("{entity:?} tags={tags:?} spatial={spatial:?}");

                if tags.is_empty() && spatial.is_empty() {
                    assert_eq!(got, Err(QueryError::Unconstrained), "{ctx}");
                    continue;
                }
                if let Some(s) = spatial.iter().find(|s| !s.applies_to(entity)) {
                    assert!(
                        matches!(got, Err(QueryError::NodesOnly { entity: e, .. }) if e == entity),
                        "{ctx}: {s:?} should be rejected, got {got:?}"
                    );
                    continue;
                }

                let mut want: Option<BTreeSet<u64>> = None;
                let constraint_sets = tags
                    .iter()
                    .map(|(k, v)| tag_by_scan(parent, entity, k, v))
                    .chain(spatial.iter().map(|s| s.by_itself(parent, entity)));
                for set in constraint_sets {
                    want = Some(match want {
                        None => set,
                        Some(prev) => prev.intersection(&set).copied().collect(),
                    });
                }
                let want: Vec<u64> = want.expect("constrained").into_iter().collect();

                assert_eq!(got.as_ref(), Ok(&want), "{ctx}");
                if !want.is_empty() {
                    non_empty += 1;
                }
            }
        }
    }
    // Guard against a fixture where everything is trivially empty.
    assert!(non_empty >= 40, "only {non_empty} non-empty cases");
}

#[test]
fn query_is_reusable_across_entity_types() {
    let archive = taginfo_archive();
    let q = archive
        .query()
        .with_tag("name", "A")
        .in_bbox(bbox(-77.05, 38.85, -76.95, 38.95));
    let parent = archive.parent();
    for (entity, got) in [
        (EntityType::Node, q.nodes()),
        (EntityType::Way, q.ways()),
        (EntityType::Relation, q.relations()),
    ] {
        let by_tag = tag_by_scan(parent, entity, "name", "A");
        let in_box = Spatial::Bbox(bbox(-77.05, 38.85, -76.95, 38.95)).by_itself(parent, entity);
        let want: Vec<u64> = by_tag.intersection(&in_box).copied().collect();
        assert_eq!(got, Ok(want), "{entity:?}");
    }
}

#[test]
fn spatial_only_query_works_without_taginfo_but_tags_need_it() {
    let archive = archive(osmflat_extc::BuildOptions {
        backrefs: true,
        ..Default::default()
    });
    let parent = archive.parent();
    let b = bbox(-77.03, 38.87, -76.99, 38.92);

    let want: Vec<u64> = query::node_indices_in_bbox(parent, b);
    assert!(!want.is_empty());
    assert_eq!(archive.query().in_bbox(b).nodes(), Ok(want));

    assert_eq!(
        archive
            .query()
            .with_tag("amenity", "cafe")
            .in_bbox(b)
            .nodes(),
        Err(QueryError::NoTaginfo)
    );
}

#[test]
fn invalid_spatial_constraints_are_errors() {
    let archive = taginfo_archive();
    let invalid = |q: query::Query| {
        assert!(
            matches!(q.nodes(), Err(QueryError::InvalidSpatial(_))),
            "expected InvalidSpatial"
        )
    };

    invalid(archive.query().within_radius(-77.0, 38.9, -0.1));
    invalid(archive.query().within_radius(f64::NAN, 38.9, 0.1));
    invalid(
        archive
            .query()
            .in_polygon(&[point(-77.0, 38.9), point(-76.9, 38.9)]),
    );
    invalid(archive.query().in_bbox(bbox(-76.9, 38.8, -77.0, 38.9)));
    invalid(archive.query().with_tag("amenity", "cafe").in_bbox(bbox(
        f64::INFINITY,
        38.8,
        -77.0,
        38.9,
    )));
}
