#![cfg(feature = "test-support")]

//! Phase 4 taginfo additions: values ordered by count, stored `key=*`
//! postings (`--key-postings`), and trigram substring search
//! (`--value-search`), each checked against brute force and against a sidecar
//! built without the addition.

use osmflat::Osm;
use osmflat_ext::query::{self, Bbox};
use osmflat_ext::ExtArchive;
use osmflat_extc::test_support::{
    build_ext_archive, build_parent_archive, Fixture, MemberSpec, NodeSpec, RelationSpec, TagSpec,
    WaySpec,
};
use std::collections::BTreeSet;

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

/// Awkward value strings: mixed case, repeats, non-ASCII, shared affixes,
/// shorter than a trigram.
const NAMES: &[&str] = &[
    "Lake Harriet",
    "LAKE STREET",
    "lakeside",
    "Bde Maka Ska",
    "Café Ñandú",
    "CAFÉ ñandú",
    "aaaa",
    "aaa",
    "aa",
    "x",
    "Main St",
    "main street",
    "121 Main",
    "12th Ave",
    "Ωmega",
];
const AMENITIES: &[&str] = &["cafe", "bar", "library", "Café"];
const HIGHWAYS: &[&str] = &["residential", "primary", "Primary Road"];

fn fixture() -> Fixture {
    let mut rng = Lcg(0x0000_ddba_11c0_ffee);
    // Skewed picks so counts vary and some tie.
    let skewed = |rng: &mut Lcg, n: usize| ((rng.next() * rng.next()) * n as f64) as usize;
    let nodes: Vec<NodeSpec> = (0..300)
        .map(|_| {
            let mut tags = Vec::new();
            if rng.next() < 0.7 {
                tags.push(TagSpec::new("name", NAMES[skewed(&mut rng, NAMES.len())]));
            }
            if rng.next() < 0.5 {
                tags.push(TagSpec::new(
                    "amenity",
                    AMENITIES[rng.below(AMENITIES.len())],
                ));
            }
            NodeSpec {
                lon: -77.05 + rng.next() * 0.1,
                lat: 38.85 + rng.next() * 0.1,
                tags,
            }
        })
        .collect();
    let ways: Vec<WaySpec> = (0..60)
        .map(|_| {
            let mut tags = vec![TagSpec::new("highway", HIGHWAYS[rng.below(HIGHWAYS.len())])];
            if rng.next() < 0.5 {
                tags.push(TagSpec::new("name", NAMES[skewed(&mut rng, NAMES.len())]));
            }
            WaySpec {
                refs: vec![rng.below(nodes.len()), rng.below(nodes.len())],
                tags,
            }
        })
        .collect();
    let relations: Vec<RelationSpec> = (0..15)
        .map(|_| {
            let (lon, lat) = (-77.05 + rng.next() * 0.09, 38.85 + rng.next() * 0.09);
            let mut tags = vec![TagSpec::new("type", "route")];
            if rng.next() < 0.6 {
                tags.push(TagSpec::new("name", NAMES[skewed(&mut rng, NAMES.len())]));
            }
            RelationSpec {
                bbox: Some((lon, lat, lon + 0.01, lat + 0.01)),
                members: vec![MemberSpec::Way(rng.below(ways.len()))],
                tags,
            }
        })
        .collect();
    Fixture {
        nodes,
        ways,
        relations,
    }
}

fn plain() -> ExtArchive {
    let parent = build_parent_archive(&fixture()).expect("parent");
    build_ext_archive(
        parent,
        &osmflat_extc::BuildOptions {
            taginfo: true,
            ..Default::default()
        },
    )
    .expect("plain sidecar")
}

fn full(scratch: Option<&tempfile::TempDir>) -> ExtArchive {
    let parent = build_parent_archive(&fixture()).expect("parent");
    build_ext_archive(
        parent,
        &osmflat_extc::BuildOptions {
            taginfo: true,
            key_postings: true,
            value_search: true,
            mmap_scratch: scratch.map(|d| d.path().to_path_buf()),
            ..Default::default()
        },
    )
    .expect("full sidecar")
}

/// Entity indices of one type carrying `key` with any value, by scanning.
fn key_by_scan(parent: &Osm, kind: usize, key: &[u8]) -> Vec<u64> {
    let tags = parent.tags();
    let tags_index = parent.tags_index();
    let strings = parent.stringtable();
    let ranges: Vec<std::ops::Range<u64>> = match kind {
        0 => parent.nodes().iter().map(|n| n.tags()).collect(),
        1 => parent.ways().iter().map(|w| w.tags()).collect(),
        _ => parent.relations().iter().map(|r| r.tags()).collect(),
    };
    ranges
        .into_iter()
        .enumerate()
        .filter(|(_, r)| {
            r.clone().any(|ti| {
                let tag = &tags[tags_index[ti as usize].value() as usize];
                strings.substring_raw(tag.key_idx() as usize) == key
            })
        })
        .map(|(i, _)| i as u64)
        .collect()
}

#[test]
fn values_by_count_orders_by_total_count_then_string() {
    for archive in [plain(), full(None)] {
        let taginfo = archive.taginfo().expect("taginfo");
        let mut ties = 0;
        for key in taginfo.keys() {
            let total = |v: &osmflat_ext::taginfo::ValueView| {
                let c = v.counts();
                c.nodes + c.ways + c.relations
            };
            let mut want: Vec<(u64, Vec<u8>)> = key
                .values()
                .map(|v| (total(&v), v.value().to_vec()))
                .collect();
            want.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let got: Vec<(u64, Vec<u8>)> = key
                .values_by_count()
                .map(|v| (total(&v), v.value().to_vec()))
                .collect();
            assert_eq!(got, want, "key {:?}", String::from_utf8_lossy(key.key()));
            ties += want.windows(2).filter(|w| w[0].0 == w[1].0).count();
            // Every value view it yields belongs to this key.
            assert!(key.values_by_count().all(|v| v.key().key() == key.key()));
        }
        assert!(ties > 0, "fixture should exercise the string tie-break");
    }
}

#[test]
fn stored_key_postings_match_merge_and_scan() {
    let scratch = tempfile::tempdir().expect("scratch");
    let (plain, full, full_mmap) = (plain(), full(None), full(Some(&scratch)));
    assert!(!plain.taginfo().unwrap().has_key_postings());
    assert!(full.taginfo().unwrap().has_key_postings());

    let boxes = [
        Bbox {
            min_lon: -77.03,
            min_lat: 38.87,
            max_lon: -76.99,
            max_lat: 38.92,
        },
        Bbox {
            min_lon: 10.0,
            min_lat: 10.0,
            max_lon: 10.1,
            max_lat: 10.1,
        },
    ];
    for key in ["name", "amenity", "highway", "type"] {
        let views =
            [&plain, &full, &full_mmap].map(|a| a.taginfo().unwrap().key(key.as_bytes()).unwrap());
        for kind in 0..3 {
            let want = key_by_scan(plain.parent(), kind, key.as_bytes());
            for view in &views {
                let got: Vec<u64> = match kind {
                    0 => view.nodes().collect(),
                    1 => view.ways().collect(),
                    _ => view.relations().collect(),
                };
                assert_eq!(got, want, "{key}=* kind {kind}");
            }
            for b in boxes {
                let got: Vec<Vec<u64>> = views
                    .iter()
                    .map(|v| match kind {
                        0 => v.nodes_in_bbox(b),
                        1 => v.ways_in_bbox(b),
                        _ => v.relations_in_bbox(b),
                    })
                    .collect();
                assert_eq!(got[0], got[1], "{key}=* kind {kind} in {b:?}");
                assert_eq!(got[0], got[2], "{key}=* kind {kind} in {b:?} (mmap)");
            }
        }
        let with_key = |a: &ExtArchive| {
            a.query()
                .with_key(key)
                .in_bbox(boxes[0])
                .nodes()
                .expect("query")
        };
        assert_eq!(with_key(&plain), with_key(&full), "Query::with_key({key})");
    }
    // A bbox-only sanity check that the stored path isn't trivially empty.
    let in_box = query::node_indices_in_bbox(full.parent(), boxes[0]);
    assert!(!full
        .taginfo()
        .unwrap()
        .key(b"name")
        .unwrap()
        .nodes_in_bbox(boxes[0])
        .is_empty());
    assert!(!in_box.is_empty());
}

/// `(key, value)` pairs of every distinct value containing `pattern`, ASCII
/// case-insensitively, in value-index order, by scanning all values.
fn containing_by_scan(archive: &ExtArchive, pattern: &[u8]) -> Vec<(Vec<u8>, Vec<u8>)> {
    let lower = |s: &[u8]| s.to_ascii_lowercase();
    let pat = lower(pattern);
    let taginfo = archive.taginfo().unwrap();
    taginfo
        .keys()
        .flat_map(|k| {
            let key = k.key().to_vec();
            k.values()
                .filter(|v| {
                    let hay = lower(v.value());
                    pat.is_empty() || hay.windows(pat.len()).any(|w| w == pat.as_slice())
                })
                .map(|v| (key.clone(), v.value().to_vec()))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn substring_search_matches_scan_with_and_without_index() {
    let scratch = tempfile::tempdir().expect("scratch");
    let (plain, full, full_mmap) = (plain(), full(None), full(Some(&scratch)));
    assert!(!plain.taginfo().unwrap().has_value_search());
    assert!(full.taginfo().unwrap().has_value_search());

    let patterns: &[&str] = &[
        "",
        "a",
        "aa",
        "aaa",
        "aaaa",
        "aaaaa",
        "ke",
        "lake",
        "LAKE",
        "Lake H",
        "ake s",
        "st",
        "street",
        "main",
        "MAIN ST",
        "12",
        "121",
        "café",
        "CAFÉ",
        "Ñandú",
        "ñandú",
        "é",
        "Ωm",
        "mega",
        "prim",
        "ROAD",
        "xyzzy",
        "Harriet!",
        "Bde Maka Ska",
        "residential",
    ];
    let pairs = |views: Vec<osmflat_ext::taginfo::ValueView>| -> Vec<(Vec<u8>, Vec<u8>)> {
        views
            .into_iter()
            .map(|v| (v.key().key().to_vec(), v.value().to_vec()))
            .collect()
    };
    let mut non_empty = 0;
    for pattern in patterns {
        let want = containing_by_scan(&plain, pattern.as_bytes());
        for (label, archive) in [
            ("scan", &plain),
            ("index", &full),
            ("index+mmap", &full_mmap),
        ] {
            let taginfo = archive.taginfo().unwrap();
            assert_eq!(
                pairs(taginfo.values_containing(pattern.as_bytes())),
                want,
                "{label}: values containing {pattern:?}"
            );
            let name = taginfo.key(b"name").unwrap();
            let want_name: Vec<_> = want.iter().filter(|(k, _)| k == b"name").cloned().collect();
            assert_eq!(
                pairs(name.values_containing(pattern.as_bytes())),
                want_name,
                "{label}: name values containing {pattern:?}"
            );
        }
        non_empty += usize::from(!want.is_empty());
    }
    assert!(
        non_empty >= 20,
        "only {non_empty} patterns matched anything"
    );

    // ASCII-only folding: 'É' and 'é' are different bytes, so they don't match.
    let full_taginfo = full.taginfo().unwrap();
    let cafe_upper: BTreeSet<Vec<u8>> = full_taginfo
        .values_containing("CAFÉ".as_bytes())
        .into_iter()
        .map(|v| v.value().to_vec())
        .collect();
    assert!(cafe_upper.contains("CAFÉ ñandú".as_bytes()));
    assert!(!cafe_upper.contains("Café Ñandú".as_bytes()));
}

/// `counts_within` / `distinct_values_within` take a bbox that was resolved to
/// ranges once, so a caller sweeping many keys pays the spatial query once
/// instead of per key. They must agree with the per-value clip they replace --
/// on both sidecars, since only `full` has stored `key=*` postings and the
/// other path sums per-value clips.
#[test]
fn counts_within_match_per_value_clip() {
    let (plain, full) = (plain(), full(None));
    assert!(!plain.taginfo().unwrap().has_key_postings());
    assert!(full.taginfo().unwrap().has_key_postings());

    let boxes = [
        // Overlaps part of the fixture.
        Bbox {
            min_lon: -77.03,
            min_lat: 38.87,
            max_lon: -76.99,
            max_lat: 38.92,
        },
        // Covers all of it.
        Bbox {
            min_lon: -78.0,
            min_lat: 38.0,
            max_lon: -76.0,
            max_lat: 39.0,
        },
        // Disjoint: every count must come back zero.
        Bbox {
            min_lon: 10.0,
            min_lat: 10.0,
            max_lon: 10.1,
            max_lat: 10.1,
        },
    ];

    let mut saw_nonzero = false;
    for archive in [&plain, &full] {
        let taginfo = archive.taginfo().unwrap();
        for b in boxes {
            let nr = query::to_index_ranges(&query::node_indices_in_bbox(archive.parent(), b));
            let wr = query::to_index_ranges(&query::way_indices_in_bbox(archive.parent(), b));
            let rr = query::to_index_ranges(&query::relation_indices_in_bbox(archive.parent(), b));

            for key in ["name", "amenity", "highway", "type"] {
                let k = taginfo.key(key.as_bytes()).unwrap();

                // The per-value sum this replaces.
                let (mut n, mut w, mut r, mut distinct) = (0u64, 0u64, 0u64, 0u64);
                for v in k.values() {
                    let vn = query::intersect_bbox(v.nodes(), &nr).count() as u64;
                    let vw = query::intersect_bbox(v.ways(), &wr).count() as u64;
                    let vr = query::intersect_bbox(v.relations(), &rr).count() as u64;
                    if vn + vw + vr > 0 {
                        distinct += 1;
                    }
                    n += vn;
                    w += vw;
                    r += vr;
                }

                let got = k.counts_within(&nr, &wr, &rr);
                assert_eq!(
                    (got.nodes, got.ways, got.relations),
                    (n, w, r),
                    "{key} in {b:?}"
                );
                assert_eq!(
                    k.distinct_values_within(&nr, &wr, &rr),
                    distinct,
                    "{key} in {b:?}"
                );
                saw_nonzero |= n + w + r > 0;
            }
        }
    }
    // Guard against the whole test passing on all-zero counts.
    assert!(saw_nonzero, "fixture produced no in-bbox matches");
}

/// A box covering everything must reproduce the archive-wide aggregates.
#[test]
fn counts_within_covering_box_equals_stored_totals() {
    let archive = full(None);
    let taginfo = archive.taginfo().unwrap();
    let b = Bbox {
        min_lon: -180.0,
        min_lat: -90.0,
        max_lon: 180.0,
        max_lat: 90.0,
    };
    let nr = query::to_index_ranges(&query::node_indices_in_bbox(archive.parent(), b));
    let wr = query::to_index_ranges(&query::way_indices_in_bbox(archive.parent(), b));
    let rr = query::to_index_ranges(&query::relation_indices_in_bbox(archive.parent(), b));

    for key in ["name", "amenity", "highway", "type"] {
        let k = taginfo.key(key.as_bytes()).unwrap();
        assert_eq!(k.counts_within(&nr, &wr, &rr), k.counts(), "{key} counts");
        assert_eq!(
            k.distinct_values_within(&nr, &wr, &rr),
            k.distinct_values(),
            "{key} distinct values"
        );
    }
}
