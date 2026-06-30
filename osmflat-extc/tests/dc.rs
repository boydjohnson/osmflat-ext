//! End-to-end test: build a Taginfo sidecar for the district-of-columbia
//! osmflat archive and check every key/value/postings against a brute-force
//! oracle that scans the parent directly.
//!
//! The archive path defaults to the sibling osmflat-rs checkout; override with
//! `OSMFLAT_DC_ARCHIVE`. The test skips (does not fail) if the archive is
//! absent, so it stays CI-safe.

use osmflat::Osm;
use osmflat_ext::{Ext, ExtArchive, FileResourceStorage};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

type Key = Vec<u8>;
type Value = Vec<u8>;

#[derive(Default)]
struct Oracle {
    // (key,value) -> ascending entity indices, per type.
    kv_nodes: HashMap<(Key, Value), Vec<u64>>,
    kv_ways: HashMap<(Key, Value), Vec<u64>>,
    kv_rels: HashMap<(Key, Value), Vec<u64>>,
}

impl Oracle {
    fn build(parent: &Osm) -> Self {
        let mut o = Oracle::default();
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
                o.kv_nodes.entry(kv(ti)).or_default().push(i as u64);
            }
        }
        for (i, way) in parent.ways().iter().enumerate() {
            for ti in way.tags() {
                o.kv_ways.entry(kv(ti)).or_default().push(i as u64);
            }
        }
        for (i, rel) in parent.relations().iter().enumerate() {
            for ti in rel.tags() {
                o.kv_rels.entry(kv(ti)).or_default().push(i as u64);
            }
        }
        o
    }

    /// Distinct keys across all (key,value) pairs of any type.
    fn distinct_keys(&self) -> HashSet<Key> {
        let mut s = HashSet::new();
        for m in [&self.kv_nodes, &self.kv_ways, &self.kv_rels] {
            for (k, _) in m.keys() {
                s.insert(k.clone());
            }
        }
        s
    }

    fn get<'a>(map: &'a HashMap<(Key, Value), Vec<u64>>, k: &[u8], v: &[u8]) -> &'a [u64] {
        map.get(&(k.to_vec(), v.to_vec()))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

fn archive_path() -> PathBuf {
    if let Ok(p) = std::env::var("OSMFLAT_DC_ARCHIVE") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../osmflat-rs/district-of-columbia.osmflat")
}

fn refs_to_vec(refs: &[osmflat_ext::Ref]) -> Vec<u64> {
    refs.iter().map(|r| r.value()).collect()
}

#[test]
fn taginfo_matches_brute_force_oracle() {
    let parent_path = archive_path();
    if !parent_path.join("header").exists() {
        eprintln!(
            "SKIP: DC archive not found at {} (set OSMFLAT_DC_ARCHIVE)",
            parent_path.display()
        );
        return;
    }

    // Build the sidecar into a fresh temp dir.
    let out = std::env::temp_dir().join(format!("osmflat-ext-dc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    osmflat_extc::build(
        &parent_path,
        &out,
        &osmflat_extc::BuildOptions {
            taginfo: true,
            ..Default::default()
        },
    )
    .expect("build taginfo sidecar");

    // Open parent + sidecar (fingerprint verified inside ExtArchive::open).
    let parent = Osm::open(FileResourceStorage::new(parent_path)).expect("open parent");
    let ext = Ext::open(FileResourceStorage::new(out.clone())).expect("open ext");
    let archive = ExtArchive::open(parent, ext).expect("fingerprint matches");

    let oracle = Oracle::build(archive.parent());
    let tq = archive.taginfo().expect("taginfo sub-archive present");

    let mut sidecar_keys: HashSet<Key> = HashSet::new();
    let mut checked_values = 0usize;

    for key_view in tq.keys() {
        let key = key_view.key().to_vec();
        assert!(sidecar_keys.insert(key.clone()), "duplicate key in sidecar");

        let (mut cn, mut cw, mut cr) = (0u64, 0u64, 0u64);
        let mut prev_value: Option<Vec<u8>> = None;

        for vv in key_view.values() {
            let value = vv.value().to_vec();

            // Values within a key are sorted ascending by string.
            if let Some(prev) = &prev_value {
                assert!(prev < &value, "values not sorted for key {key:?}");
            }
            prev_value = Some(value.clone());

            let nodes = refs_to_vec(vv.nodes());
            let ways = refs_to_vec(vv.ways());
            let rels = refs_to_vec(vv.relations());

            // Postings equal the oracle's, in the same ascending order.
            assert_eq!(
                nodes,
                Oracle::get(&oracle.kv_nodes, &key, &value),
                "nodes for {key:?}={value:?}"
            );
            assert_eq!(
                ways,
                Oracle::get(&oracle.kv_ways, &key, &value),
                "ways for {key:?}={value:?}"
            );
            assert_eq!(
                rels,
                Oracle::get(&oracle.kv_rels, &key, &value),
                "rels for {key:?}={value:?}"
            );

            // Postings ascending (spatial-order invariant).
            assert!(nodes.windows(2).all(|w| w[0] < w[1]));

            // Derived per-(k,v) counts == postings lengths.
            let counts = vv.counts();
            assert_eq!(counts.nodes, nodes.len() as u64);
            assert_eq!(counts.ways, ways.len() as u64);
            assert_eq!(counts.relations, rels.len() as u64);

            cn += counts.nodes;
            cw += counts.ways;
            cr += counts.relations;
            checked_values += 1;
        }

        // Key-level aggregate counts.
        let kc = key_view.counts();
        assert_eq!(
            (kc.nodes, kc.ways, kc.relations),
            (cn, cw, cr),
            "key counts {key:?}"
        );
    }

    // Same set of distinct keys, and direct lookup round-trips.
    assert_eq!(
        sidecar_keys,
        oracle.distinct_keys(),
        "distinct key set differs"
    );
    assert!(checked_values > 0, "no values checked — archive empty?");

    // Spot-check a direct (key,value) lookup if `highway` exists.
    if let Some(highway) = tq.key(b"highway") {
        assert_eq!(highway.key(), b"highway");
        for vv in highway.values() {
            let v = vv.value().to_vec();
            assert!(
                tq.kv(b"highway", &v).is_some(),
                "kv lookup failed for highway={v:?}"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&out);
    eprintln!(
        "OK: {} keys, {} (key,value) pairs verified against oracle",
        sidecar_keys.len(),
        checked_values
    );
}
