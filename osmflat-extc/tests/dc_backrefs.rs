//! End-to-end test: build a Backrefs sidecar for the district-of-columbia
//! osmflat archive and check every reverse-reference list against a brute-force
//! oracle that scans the parent directly.
//!
//! Skips (does not fail) if the archive is absent. Override the path with
//! `OSMFLAT_DC_ARCHIVE`.

use osmflat::{Osm, RelationMembersRef};
use osmflat_ext::{Ext, ExtArchive, FileResourceStorage, Ref};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Default)]
struct Oracle {
    ways_of_node: HashMap<u64, Vec<u64>>,
    rels_of_node: HashMap<u64, Vec<u64>>,
    rels_of_way: HashMap<u64, Vec<u64>>,
    rels_of_rel: HashMap<u64, Vec<u64>>,
}

/// Append `x` to `map[key]` unless it is already the last element (mirrors the
/// compiler's consecutive-duplicate dedup, preserving ascending order).
fn push_distinct(map: &mut HashMap<u64, Vec<u64>>, key: u64, x: u64) {
    let list = map.entry(key).or_default();
    if list.last() != Some(&x) {
        list.push(x);
    }
}

impl Oracle {
    fn build(parent: &Osm) -> Self {
        let mut o = Oracle::default();

        let nodes_index = parent.nodes_index();
        for (w, way) in parent.ways().iter().enumerate() {
            for ri in way.refs() {
                if let Some(n) = nodes_index[ri as usize].value() {
                    push_distinct(&mut o.ways_of_node, n, w as u64);
                }
            }
        }

        let members = parent.relation_members();
        for r in 0..parent.relations().len() {
            for member in members.at(r) {
                match member {
                    RelationMembersRef::NodeMember(m) => {
                        if let Some(n) = m.node_idx() {
                            push_distinct(&mut o.rels_of_node, n, r as u64);
                        }
                    }
                    RelationMembersRef::WayMember(m) => {
                        if let Some(w) = m.way_idx() {
                            push_distinct(&mut o.rels_of_way, w, r as u64);
                        }
                    }
                    RelationMembersRef::RelationMember(m) => {
                        if let Some(rr) = m.relation_idx() {
                            push_distinct(&mut o.rels_of_rel, rr, r as u64);
                        }
                    }
                }
            }
        }
        o
    }
}

fn archive_path() -> PathBuf {
    if let Ok(p) = std::env::var("OSMFLAT_DC_ARCHIVE") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../osmflat-rs/district-of-columbia.osmflat")
}

fn refs(refs: &[Ref]) -> Vec<u64> {
    refs.iter().map(|r| r.value()).collect()
}

/// Assert a backref list equals the oracle's and is strictly ascending.
fn check(got: Vec<u64>, want: Option<&Vec<u64>>, ctx: &str) {
    let want = want.cloned().unwrap_or_default();
    assert_eq!(got, want, "{ctx}");
    assert!(
        got.windows(2).all(|w| w[0] < w[1]),
        "{ctx}: not ascending/distinct"
    );
}

#[test]
fn backrefs_match_brute_force_oracle() {
    let parent_path = archive_path();
    if !parent_path.join("header").exists() {
        eprintln!(
            "SKIP: DC archive not found at {} (set OSMFLAT_DC_ARCHIVE)",
            parent_path.display()
        );
        return;
    }

    let out = std::env::temp_dir().join(format!("osmflat-ext-dc-br-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    osmflat_extc::build(
        &parent_path,
        &out,
        &osmflat_extc::BuildOptions {
            backrefs: true,
            ..Default::default()
        },
    )
    .expect("build backrefs sidecar");

    let parent = Osm::open(FileResourceStorage::new(parent_path)).expect("open parent");
    let ext = Ext::open(FileResourceStorage::new(out.clone())).expect("open ext");
    let archive = ExtArchive::open(parent, ext).expect("fingerprint matches");

    let oracle = Oracle::build(archive.parent());
    let br = archive.backrefs().expect("backrefs sub-archive present");

    let (n_nodes, n_ways, n_rels) = (
        archive.parent().nodes().len(),
        archive.parent().ways().len(),
        archive.parent().relations().len(),
    );

    for n in 0..n_nodes {
        let i = n as u64;
        check(
            refs(br.ways_using_node(n)),
            oracle.ways_of_node.get(&i),
            "ways_using_node",
        );
        check(
            refs(br.relations_with_node(n)),
            oracle.rels_of_node.get(&i),
            "relations_with_node",
        );
    }
    for w in 0..n_ways {
        let i = w as u64;
        check(
            refs(br.relations_with_way(w)),
            oracle.rels_of_way.get(&i),
            "relations_with_way",
        );
    }
    for r in 0..n_rels {
        let i = r as u64;
        check(
            refs(br.relations_with_relation(r)),
            oracle.rels_of_rel.get(&i),
            "relations_with_relation",
        );
    }

    let _ = std::fs::remove_dir_all(&out);
    eprintln!(
        "OK: backrefs verified — {} nodes, {} ways, {} relations; {} node->way edges",
        n_nodes,
        n_ways,
        n_rels,
        oracle.ways_of_node.values().map(Vec::len).sum::<usize>(),
    );
}
