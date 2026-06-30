#![cfg(feature = "test-support")]

use osmflat::{Osm, RelationMembersRef};
use osmflat_ext::query::{self, Bbox};
use osmflat_ext::{ExtArchive, Ref};
use osmflat_extc::test_support::{
    build_ext_archive, build_parent_archive, Fixture, MemberSpec, NodeSpec, RelationSpec, TagSpec,
    WaySpec,
};
use std::collections::{HashMap, HashSet};

type Key = Vec<u8>;
type Value = Vec<u8>;

const BBOX: Bbox = Bbox {
    min_lon: -77.04,
    min_lat: 38.89,
    max_lon: -77.01,
    max_lat: 38.91,
};

fn tag(key: &'static str, value: &'static str) -> TagSpec {
    TagSpec::new(key, value)
}

fn fixture() -> Fixture {
    Fixture {
        nodes: vec![
            NodeSpec {
                lon: -77.05,
                lat: 38.88,
                tags: vec![tag("amenity", "cafe"), tag("name", "West")],
            },
            NodeSpec {
                lon: -77.03,
                lat: 38.90,
                tags: vec![tag("amenity", "cafe"), tag("name", "Alpha")],
            },
            NodeSpec {
                lon: -77.025,
                lat: 38.905,
                tags: vec![tag("amenity", "cafe"), tag("highway", "crossing")],
            },
            NodeSpec {
                lon: -77.015,
                lat: 38.895,
                tags: vec![tag("highway", "bus_stop"), tag("shelter", "yes")],
            },
            NodeSpec {
                lon: -77.00,
                lat: 38.92,
                tags: vec![tag("amenity", "cafe"), tag("name", "East")],
            },
            NodeSpec {
                lon: -77.02,
                lat: 38.902,
                tags: vec![tag("shop", "books"), tag("name", "Central")],
            },
        ],
        ways: vec![
            WaySpec {
                refs: vec![1, 2],
                tags: vec![tag("highway", "residential"), tag("name", "Main")],
            },
            WaySpec {
                refs: vec![0, 4],
                tags: vec![tag("highway", "primary"), tag("name", "Long")],
            },
            WaySpec {
                refs: vec![2, 5],
                tags: vec![tag("building", "yes"), tag("amenity", "cafe")],
            },
        ],
        relations: vec![
            RelationSpec {
                bbox: Some((-77.031, 38.899, -77.024, 38.906)),
                members: vec![MemberSpec::Node(1), MemberSpec::Way(0), MemberSpec::Node(1)],
                tags: vec![tag("type", "route"), tag("route", "bus")],
            },
            RelationSpec {
                bbox: None,
                members: vec![
                    MemberSpec::Way(1),
                    MemberSpec::Relation(0),
                    MemberSpec::Relation(0),
                ],
                tags: vec![tag("type", "multipolygon"), tag("name", "No Location")],
            },
            RelationSpec {
                bbox: Some((-77.026, 38.901, -77.019, 38.906)),
                members: vec![
                    MemberSpec::Node(2),
                    MemberSpec::Way(2),
                    MemberSpec::Relation(0),
                ],
                tags: vec![tag("type", "site"), tag("amenity", "cafe")],
            },
        ],
    }
}

fn archive(opts: osmflat_extc::BuildOptions) -> ExtArchive {
    let parent = build_parent_archive(&fixture()).expect("build synthetic parent archive");
    build_ext_archive(parent, &opts).expect("build synthetic extension archive")
}

#[derive(Default)]
struct TagOracle {
    kv_nodes: HashMap<(Key, Value), Vec<u64>>,
    kv_ways: HashMap<(Key, Value), Vec<u64>>,
    kv_rels: HashMap<(Key, Value), Vec<u64>>,
}

impl TagOracle {
    fn build(parent: &Osm) -> Self {
        let mut oracle = TagOracle::default();
        let tags = parent.tags();
        let tags_index = parent.tags_index();
        let strings = parent.stringtable();
        let kv = |ti: u64| {
            let tag = &tags[tags_index[ti as usize].value() as usize];
            (
                strings.substring_raw(tag.key_idx() as usize).to_vec(),
                strings.substring_raw(tag.value_idx() as usize).to_vec(),
            )
        };
        for (i, node) in parent.nodes().iter().enumerate() {
            for ti in node.tags() {
                oracle.kv_nodes.entry(kv(ti)).or_default().push(i as u64);
            }
        }
        for (i, way) in parent.ways().iter().enumerate() {
            for ti in way.tags() {
                oracle.kv_ways.entry(kv(ti)).or_default().push(i as u64);
            }
        }
        for (i, rel) in parent.relations().iter().enumerate() {
            for ti in rel.tags() {
                oracle.kv_rels.entry(kv(ti)).or_default().push(i as u64);
            }
        }
        oracle
    }

    fn distinct_keys(&self) -> HashSet<Key> {
        let mut keys = HashSet::new();
        for map in [&self.kv_nodes, &self.kv_ways, &self.kv_rels] {
            for (key, _) in map.keys() {
                keys.insert(key.clone());
            }
        }
        keys
    }

    fn get<'a>(map: &'a HashMap<(Key, Value), Vec<u64>>, key: &[u8], value: &[u8]) -> &'a [u64] {
        map.get(&(key.to_vec(), value.to_vec()))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

fn refs_to_vec(refs: &[Ref]) -> Vec<u64> {
    refs.iter().map(|r| r.value()).collect()
}

#[test]
fn taginfo_matches_brute_force_oracle() {
    let archive = archive(osmflat_extc::BuildOptions {
        taginfo: true,
        ..Default::default()
    });
    let oracle = TagOracle::build(archive.parent());
    let taginfo = archive.taginfo().expect("taginfo sub-archive present");

    let mut sidecar_keys = HashSet::new();
    let mut checked_values = 0usize;

    for key_view in taginfo.keys() {
        let key = key_view.key().to_vec();
        assert!(sidecar_keys.insert(key.clone()), "duplicate sidecar key");

        let mut previous_value: Option<Vec<u8>> = None;
        let (mut count_nodes, mut count_ways, mut count_rels) = (0, 0, 0);

        for value_view in key_view.values() {
            let value = value_view.value().to_vec();
            if let Some(previous) = &previous_value {
                assert!(previous < &value, "values not sorted for key {key:?}");
            }
            previous_value = Some(value.clone());

            let nodes = refs_to_vec(value_view.nodes());
            let ways = refs_to_vec(value_view.ways());
            let rels = refs_to_vec(value_view.relations());

            assert_eq!(
                nodes,
                TagOracle::get(&oracle.kv_nodes, &key, &value),
                "node postings for {key:?}={value:?}"
            );
            assert_eq!(
                ways,
                TagOracle::get(&oracle.kv_ways, &key, &value),
                "way postings for {key:?}={value:?}"
            );
            assert_eq!(
                rels,
                TagOracle::get(&oracle.kv_rels, &key, &value),
                "relation postings for {key:?}={value:?}"
            );

            assert!(nodes.windows(2).all(|w| w[0] < w[1]));
            assert!(ways.windows(2).all(|w| w[0] < w[1]));
            assert!(rels.windows(2).all(|w| w[0] < w[1]));

            let counts = value_view.counts();
            assert_eq!(counts.nodes, nodes.len() as u64);
            assert_eq!(counts.ways, ways.len() as u64);
            assert_eq!(counts.relations, rels.len() as u64);

            count_nodes += counts.nodes;
            count_ways += counts.ways;
            count_rels += counts.relations;
            checked_values += 1;
        }

        let key_counts = key_view.counts();
        assert_eq!(
            (key_counts.nodes, key_counts.ways, key_counts.relations),
            (count_nodes, count_ways, count_rels),
            "aggregate counts for key {key:?}"
        );
    }

    assert_eq!(sidecar_keys, oracle.distinct_keys());
    assert!(checked_values > 0, "no values checked");
    for key_view in taginfo.keys() {
        assert_eq!(
            key_view.combinations().count(),
            0,
            "combinations should be empty without --combinations"
        );
    }

    let amenity_cafe = taginfo
        .kv(b"amenity", b"cafe")
        .expect("amenity=cafe should exist");
    assert_eq!(
        refs_to_vec(amenity_cafe.nodes()),
        TagOracle::get(&oracle.kv_nodes, b"amenity", b"cafe")
    );
}

#[derive(Default)]
struct CombinationOracle {
    by_key: HashMap<Key, Vec<(Key, u64)>>,
}

impl CombinationOracle {
    fn build(parent: &Osm) -> Self {
        let mut counts: HashMap<(Key, Key), u64> = HashMap::new();

        for keys in entity_keys(parent) {
            for key in &keys {
                for other_key in &keys {
                    if key != other_key {
                        *counts.entry((key.clone(), other_key.clone())).or_default() += 1;
                    }
                }
            }
        }

        let mut by_key: HashMap<Key, Vec<(Key, u64)>> = HashMap::new();
        for ((key, other_key), together_count) in counts {
            by_key
                .entry(key)
                .or_default()
                .push((other_key, together_count));
        }
        for combos in by_key.values_mut() {
            combos.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        }

        CombinationOracle { by_key }
    }
}

fn entity_keys(parent: &Osm) -> Vec<Vec<Key>> {
    let tags = parent.tags();
    let tags_index = parent.tags_index();
    let strings = parent.stringtable();
    let mut out = Vec::new();

    for range in parent
        .nodes()
        .iter()
        .map(|node| node.tags())
        .chain(parent.ways().iter().map(|way| way.tags()))
        .chain(parent.relations().iter().map(|relation| relation.tags()))
    {
        let mut keys: Vec<Key> = range
            .map(|tag_index_idx| {
                let tag = &tags[tags_index[tag_index_idx as usize].value() as usize];
                strings.substring_raw(tag.key_idx() as usize).to_vec()
            })
            .collect();
        keys.sort();
        keys.dedup();
        out.push(keys);
    }

    out
}

#[test]
fn combinations_match_brute_force_oracle() {
    let archive = archive(osmflat_extc::BuildOptions {
        combinations: true,
        ..Default::default()
    });
    let oracle = CombinationOracle::build(archive.parent());
    let taginfo = archive.taginfo().expect("taginfo sub-archive present");

    for key_view in taginfo.keys() {
        let key = key_view.key().to_vec();
        let got: Vec<(Key, u64)> = key_view
            .combinations()
            .map(|combo| (combo.key().to_vec(), combo.together_count()))
            .collect();
        let want = oracle.by_key.get(&key).cloned().unwrap_or_default();
        assert_eq!(got, want, "combinations for key {key:?}");
    }

    let amenity = taginfo.key(b"amenity").expect("amenity key exists");
    let combos: Vec<(Key, u64)> = amenity
        .combinations()
        .map(|combo| (combo.key().to_vec(), combo.together_count()))
        .collect();
    assert!(
        combos
            .iter()
            .any(|(key, count)| key.as_slice() == b"name" && *count == 3),
        "amenity should co-occur with name on three entities"
    );
}

#[derive(Default)]
struct BackrefsOracle {
    ways_of_node: HashMap<u64, Vec<u64>>,
    rels_of_node: HashMap<u64, Vec<u64>>,
    rels_of_way: HashMap<u64, Vec<u64>>,
    rels_of_rel: HashMap<u64, Vec<u64>>,
}

fn push_distinct(map: &mut HashMap<u64, Vec<u64>>, key: u64, value: u64) {
    let list = map.entry(key).or_default();
    if list.last() != Some(&value) {
        list.push(value);
    }
}

impl BackrefsOracle {
    fn build(parent: &Osm) -> Self {
        let mut oracle = BackrefsOracle::default();

        let nodes_index = parent.nodes_index();
        for (way_idx, way) in parent.ways().iter().enumerate() {
            for ref_idx in way.refs() {
                if let Some(node_idx) = nodes_index[ref_idx as usize].value() {
                    push_distinct(&mut oracle.ways_of_node, node_idx, way_idx as u64);
                }
            }
        }

        let members = parent.relation_members();
        for relation_idx in 0..parent.relations().len() {
            for member in members.at(relation_idx) {
                match member {
                    RelationMembersRef::NodeMember(member) => {
                        if let Some(node_idx) = member.node_idx() {
                            push_distinct(&mut oracle.rels_of_node, node_idx, relation_idx as u64);
                        }
                    }
                    RelationMembersRef::WayMember(member) => {
                        if let Some(way_idx) = member.way_idx() {
                            push_distinct(&mut oracle.rels_of_way, way_idx, relation_idx as u64);
                        }
                    }
                    RelationMembersRef::RelationMember(member) => {
                        if let Some(child_relation_idx) = member.relation_idx() {
                            push_distinct(
                                &mut oracle.rels_of_rel,
                                child_relation_idx,
                                relation_idx as u64,
                            );
                        }
                    }
                }
            }
        }

        oracle
    }
}

fn check_refs(got: Vec<u64>, want: Option<&Vec<u64>>, ctx: &str) {
    let want = want.cloned().unwrap_or_default();
    assert_eq!(got, want, "{ctx}");
    assert!(
        got.windows(2).all(|w| w[0] < w[1]),
        "{ctx}: postings are not ascending and distinct"
    );
}

#[test]
fn backrefs_match_brute_force_oracle() {
    let archive = archive(osmflat_extc::BuildOptions {
        backrefs: true,
        ..Default::default()
    });
    let oracle = BackrefsOracle::build(archive.parent());
    let backrefs = archive.backrefs().expect("backrefs sub-archive present");

    for node_idx in 0..archive.parent().nodes().len() {
        let idx = node_idx as u64;
        check_refs(
            refs_to_vec(backrefs.ways_using_node(node_idx)),
            oracle.ways_of_node.get(&idx),
            "ways_using_node",
        );
        check_refs(
            refs_to_vec(backrefs.relations_with_node(node_idx)),
            oracle.rels_of_node.get(&idx),
            "relations_with_node",
        );
    }

    for way_idx in 0..archive.parent().ways().len() {
        let idx = way_idx as u64;
        check_refs(
            refs_to_vec(backrefs.relations_with_way(way_idx)),
            oracle.rels_of_way.get(&idx),
            "relations_with_way",
        );
    }

    for relation_idx in 0..archive.parent().relations().len() {
        let idx = relation_idx as u64;
        check_refs(
            refs_to_vec(backrefs.relations_with_relation(relation_idx)),
            oracle.rels_of_rel.get(&idx),
            "relations_with_relation",
        );
    }
}

fn nodes_in_box_by_scan(parent: &Osm) -> Vec<u64> {
    let scale = parent.header().coord_scale() as f64;
    parent
        .nodes()
        .iter()
        .enumerate()
        .filter_map(|(idx, node)| {
            let lon = node.lon() as f64 / scale;
            let lat = node.lat() as f64 / scale;
            (lon >= BBOX.min_lon
                && lon <= BBOX.max_lon
                && lat >= BBOX.min_lat
                && lat <= BBOX.max_lat)
                .then_some(idx as u64)
        })
        .collect()
}

fn intersect_sorted(a: &[u64], b: &[u64]) -> Vec<u64> {
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    out
}

#[test]
fn key_value_intersect_bbox_merge_join() {
    let archive = archive(osmflat_extc::BuildOptions {
        taginfo: true,
        ..Default::default()
    });
    let taginfo = archive.taginfo().expect("taginfo sub-archive present");

    let box_nodes = nodes_in_box_by_scan(archive.parent());
    let curve_nodes = query::node_indices_in_bbox(archive.parent(), BBOX);
    assert_eq!(curve_nodes, box_nodes);
    assert!(!box_nodes.is_empty(), "bbox should contain fixture nodes");

    let box_set: HashSet<u64> = box_nodes.iter().copied().collect();
    let amenity_cafe = taginfo
        .kv(b"amenity", b"cafe")
        .expect("amenity=cafe should exist");
    let want_nodes: Vec<u64> = amenity_cafe
        .nodes()
        .iter()
        .map(|r| r.value())
        .filter(|idx| box_set.contains(idx))
        .collect();
    assert_eq!(amenity_cafe.nodes_in_bbox(BBOX), want_nodes);
    assert!(!want_nodes.is_empty(), "amenity=cafe should hit bbox");

    let node_ranges = query::to_index_ranges(&box_nodes);
    let intersected: Vec<u64> = query::intersect_bbox(amenity_cafe.nodes(), &node_ranges).collect();
    assert_eq!(intersected, want_nodes);

    let residential = taginfo
        .kv(b"highway", b"residential")
        .expect("highway=residential should exist");
    let box_ways = query::way_indices_in_bbox(archive.parent(), BBOX);
    let global_ways = refs_to_vec(residential.ways());
    let want_ways = intersect_sorted(&global_ways, &box_ways);
    assert_eq!(residential.ways_in_bbox(BBOX), want_ways);
    assert!(!want_ways.is_empty(), "highway=residential should hit bbox");
}
