//! End-to-end test of the `key=value ∩ bbox` merge-join on the
//! district-of-columbia archive.
//!
//! The node case is checked against a fully independent oracle (a point-in-box
//! scan over every node), which validates the whole chain: osmflat's spatial
//! query, index recovery, run-length range compression, and the
//! `intersect_bbox` merge-join. The way case is checked against the parent's
//! own bbox result intersected with the tag postings.
//!
//! Skips if the archive is absent. Override the path with `OSMFLAT_DC_ARCHIVE`.

use osmflat::Osm;
use osmflat_ext::query::{self, Bbox};
use osmflat_ext::{Ext, ExtArchive, FileResourceStorage};
use std::collections::HashMap;
use std::path::PathBuf;

// A downtown-DC sub-box (degrees) — small enough to actually filter, dense
// enough to contain plenty of tagged nodes and ways.
const BBOX: Bbox = Bbox {
    min_lon: -77.04,
    min_lat: 38.89,
    max_lon: -77.01,
    max_lat: 38.91,
};

fn archive_path() -> PathBuf {
    if let Ok(p) = std::env::var("OSMFLAT_DC_ARCHIVE") {
        return PathBuf::from(p);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../osmflat-rs/district-of-columbia.osmflat")
}

/// Every node index whose coordinates fall inside `BBOX`, by a direct scan —
/// independent of osmflat's space-filling-curve query.
fn nodes_in_box_by_scan(parent: &Osm) -> Vec<u64> {
    let scale = parent.header().coord_scale() as f64;
    let mut out = Vec::new();
    for (i, node) in parent.nodes().iter().enumerate() {
        let lon = node.lon() as f64 / scale;
        let lat = node.lat() as f64 / scale;
        if lon >= BBOX.min_lon && lon <= BBOX.max_lon && lat >= BBOX.min_lat && lat <= BBOX.max_lat
        {
            out.push(i as u64);
        }
    }
    out
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
    let parent_path = archive_path();
    if !parent_path.join("header").exists() {
        eprintln!(
            "SKIP: DC archive not found at {} (set OSMFLAT_DC_ARCHIVE)",
            parent_path.display()
        );
        return;
    }

    let out = std::env::temp_dir().join(format!("osmflat-ext-dc-bbox-{}", std::process::id()));
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

    let parent = Osm::open(FileResourceStorage::new(parent_path)).expect("open parent");
    let ext = Ext::open(FileResourceStorage::new(out.clone())).expect("open ext");
    let archive = ExtArchive::open(parent, ext).expect("fingerprint matches");
    let tq = archive.taginfo().expect("taginfo present");

    // (1) The bbox machinery itself: osmflat's curve query must agree with the
    // independent point-in-box scan.
    let box_nodes = nodes_in_box_by_scan(archive.parent());
    let curve_nodes = query::node_indices_in_bbox(archive.parent(), BBOX);
    assert_eq!(
        curve_nodes, box_nodes,
        "node_indices_in_bbox disagrees with point-in-box scan"
    );
    assert!(
        !box_nodes.is_empty(),
        "bbox contains no nodes — bad test box?"
    );

    // (2) Build an independent oracle: for each (key,value), the in-box nodes
    // carrying it, ascending. Pick the densest to exercise a non-trivial result.
    let box_set: std::collections::HashSet<u64> = box_nodes.iter().copied().collect();
    let strings = archive.parent().stringtable();
    let tags = archive.parent().tags();
    let tags_index = archive.parent().tags_index();
    let mut oracle: HashMap<(Vec<u8>, Vec<u8>), Vec<u64>> = HashMap::new();
    for (i, node) in archive.parent().nodes().iter().enumerate() {
        if !box_set.contains(&(i as u64)) {
            continue;
        }
        for ti in node.tags() {
            let tag = &tags[tags_index[ti as usize].value() as usize];
            let k = strings.substring_raw(tag.key_idx() as usize).to_vec();
            let v = strings.substring_raw(tag.value_idx() as usize).to_vec();
            oracle.entry((k, v)).or_default().push(i as u64);
        }
    }

    let (dense_kv, dense_nodes) = oracle
        .iter()
        .max_by_key(|(_, nodes)| nodes.len())
        .map(|(kv, nodes)| (kv.clone(), nodes.clone()))
        .expect("at least one tagged node in box");

    // (3) The merge-join must reproduce the oracle exactly for the densest kv...
    let vv = tq
        .kv(&dense_kv.0, &dense_kv.1)
        .expect("densest kv present in sidecar");
    let merged = vv.nodes_in_bbox(BBOX);
    assert_eq!(
        merged, dense_nodes,
        "nodes_in_bbox != oracle for densest kv"
    );
    assert!(
        merged.windows(2).all(|w| w[0] < w[1]),
        "result not ascending"
    );
    // ...and be a subset of the kv's global node postings.
    let global: Vec<u64> = vv.nodes().iter().map(|r| r.value()).collect();
    assert_eq!(intersect_sorted(&global, &box_nodes), merged);

    // (4) Spot-check many kvs against (global postings ∩ box). Reuses the
    // already-computed box ranges and drives `intersect_bbox` directly, so the
    // bbox query runs once (in step 1), not per-kv.
    let node_ranges = query::to_index_ranges(&box_nodes);
    let mut checked = 0usize;
    'outer: for key_view in tq.keys() {
        for value_view in key_view.values() {
            let postings = value_view.nodes();
            // O(postings) reference via box_set membership.
            let want: Vec<u64> = postings
                .iter()
                .map(|r| r.value())
                .filter(|i| box_set.contains(i))
                .collect();
            let got: Vec<u64> = query::intersect_bbox(postings, &node_ranges).collect();
            assert_eq!(got, want, "intersect_bbox mismatch for a sampled kv");
            checked += 1;
            if checked >= 5000 {
                break 'outer;
            }
        }
    }

    // (5) Ways: full-path merge-join equals (way postings ∩ parent bbox ways).
    // Pick the densest-by-global-count way kv (cheap: just postings lengths),
    // then verify the full `ways_in_bbox` path once.
    let box_ways = query::way_indices_in_bbox(archive.parent(), BBOX);
    let mut way_kv: Option<(Vec<u8>, Vec<u8>)> = None;
    let mut best_ways = 0usize;
    for key_view in tq.keys() {
        for value_view in key_view.values() {
            let n = value_view.ways().len();
            if n > best_ways {
                best_ways = n;
                way_kv = Some((key_view.key().to_vec(), value_view.value().to_vec()));
            }
        }
    }
    let way_kv = way_kv.expect("some way kv exists");
    let wv = tq.kv(&way_kv.0, &way_kv.1).unwrap();
    let want_ways = intersect_sorted(
        &wv.ways().iter().map(|r| r.value()).collect::<Vec<_>>(),
        &box_ways,
    );
    assert_eq!(wv.ways_in_bbox(BBOX), want_ways, "ways_in_bbox mismatch");
    assert!(!want_ways.is_empty(), "densest way kv has no ways in box");

    let _ = std::fs::remove_dir_all(&out);
    eprintln!(
        "OK: bbox merge-join — {} nodes in box; densest node kv {:?}={:?} -> {} in-box; \
         {} ways in box, densest way kv {:?}={:?} -> {} in-box; {} kvs spot-checked",
        box_nodes.len(),
        String::from_utf8_lossy(&dense_kv.0),
        String::from_utf8_lossy(&dense_kv.1),
        merged.len(),
        box_ways.len(),
        String::from_utf8_lossy(&way_kv.0),
        String::from_utf8_lossy(&way_kv.1),
        want_ways.len(),
        checked,
    );
}
