#![cfg(feature = "test-support")]

use osmflat::test_support::build_archive;
use osmflat::Osm;
use osmflat_ext::spatial::{k_nearest_nodes, nodes_in_polygon, nodes_within_radius, Point};

fn archive() -> Osm {
    build_archive(
        &[
            (-77.050, 38.880),
            (-77.030, 38.900),
            (-77.025, 38.905),
            (-77.015, 38.895),
            (-77.000, 38.920),
            (-77.020, 38.902),
        ],
        &[],
        &[],
    )
}

fn distance_sq(a: Point, b: Point) -> f64 {
    let dx = a.lon - b.lon;
    let dy = a.lat - b.lat;
    dx * dx + dy * dy
}

fn node_point(archive: &Osm, idx: usize) -> Point {
    let node = &archive.nodes()[idx];
    let scale = archive.header().coord_scale() as f64;
    Point {
        lon: node.lon() as f64 / scale,
        lat: node.lat() as f64 / scale,
    }
}

#[test]
fn nodes_within_radius_matches_exact_scan_nearest_first() {
    let archive = archive();
    let center = Point {
        lon: -77.025,
        lat: 38.902,
    };
    let radius = 0.011;
    let radius_sq = radius * radius;

    let mut expected: Vec<(f64, usize)> = archive
        .nodes()
        .iter()
        .enumerate()
        .filter_map(|(idx, _)| {
            let dist = distance_sq(center, node_point(&archive, idx));
            (dist <= radius_sq).then_some((dist, idx))
        })
        .collect();
    expected.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let expected: Vec<usize> = expected.into_iter().map(|(_, idx)| idx).collect();

    let got: Vec<usize> = nodes_within_radius(&archive, center.lon, center.lat, radius).collect();
    assert_eq!(got, expected);
    assert!(nodes_within_radius(&archive, center.lon, center.lat, -1.0)
        .next()
        .is_none());
}

#[test]
fn k_nearest_nodes_matches_exact_scan() {
    let archive = archive();
    let center = Point {
        lon: -77.024,
        lat: 38.901,
    };

    let mut expected: Vec<(f64, usize)> = archive
        .nodes()
        .iter()
        .enumerate()
        .map(|(idx, _)| (distance_sq(center, node_point(&archive, idx)), idx))
        .collect();
    expected.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    assert_eq!(
        k_nearest_nodes(&archive, center.lon, center.lat, 0),
        Vec::<usize>::new()
    );
    assert_eq!(
        k_nearest_nodes(&archive, center.lon, center.lat, 3),
        expected
            .iter()
            .take(3)
            .map(|&(_, idx)| idx)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        k_nearest_nodes(&archive, center.lon, center.lat, archive.nodes().len() + 10),
        expected.iter().map(|&(_, idx)| idx).collect::<Vec<_>>()
    );
}

/// Exact k-NN oracle in archive units (same truncating scale and tie-break by
/// index as the library), so float rounding can't disagree about ties.
fn k_nearest_oracle(archive: &Osm, center: Point, k: usize) -> Vec<usize> {
    let scale = archive.header().coord_scale() as f64;
    let (cx, cy) = (
        (center.lon * scale) as i32 as i128,
        (center.lat * scale) as i32 as i128,
    );
    let mut all: Vec<(i128, usize)> = archive
        .nodes()
        .iter()
        .enumerate()
        .map(|(idx, n)| {
            let (dx, dy) = (n.lon() as i128 - cx, n.lat() as i128 - cy);
            (dx * dx + dy * dy, idx)
        })
        .collect();
    all.sort_unstable();
    all.into_iter().take(k).map(|(_, idx)| idx).collect()
}

#[test]
fn k_nearest_nodes_matches_oracle_across_densities_ties_and_sparse_centers() {
    // A dense ~1 m grid (many exact distance ties), a scattered ~km cloud, and
    // a few far-flung outliers so some searches must expand to the whole world.
    let mut coords = Vec::new();
    for i in 0..12 {
        for j in 0..12 {
            coords.push((-77.0 + i as f64 * 0.00001, 38.9 + j as f64 * 0.00001));
        }
    }
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = || {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 11) as f64 / (1u64 << 53) as f64
    };
    for _ in 0..200 {
        coords.push((-77.05 + next() * 0.1, 38.85 + next() * 0.1));
    }
    coords.extend([
        (151.2, -33.9),
        (-0.12, 51.5),
        (179.9, 89.9),
        (-179.9, -89.9),
    ]);
    let archive = build_archive(&coords, &[], &[]);

    let centers = [
        // middle of the grid
        Point {
            lon: -76.99995,
            lat: 38.90005,
        },
        // exactly on a grid node
        Point {
            lon: -77.0,
            lat: 38.9,
        },
        // inside the scattered cloud
        Point {
            lon: -77.02,
            lat: 38.88,
        },
        // far from everything
        Point {
            lon: 10.0,
            lat: -45.0,
        },
        // world corner
        Point {
            lon: 180.0,
            lat: 90.0,
        },
    ];
    for center in centers {
        for k in [1, 2, 5, 17, 150, 300, coords.len(), coords.len() + 5] {
            assert_eq!(
                k_nearest_nodes(&archive, center.lon, center.lat, k),
                k_nearest_oracle(&archive, center, k),
                "center=({}, {}) k={k}",
                center.lon,
                center.lat
            );
        }
    }
}

#[test]
fn nodes_in_polygon_matches_rectangle_scan_and_includes_boundary() {
    let archive = archive();
    let polygon = [
        Point {
            lon: -77.030,
            lat: 38.895,
        },
        Point {
            lon: -77.015,
            lat: 38.895,
        },
        Point {
            lon: -77.015,
            lat: 38.905,
        },
        Point {
            lon: -77.030,
            lat: 38.905,
        },
    ];

    let expected: Vec<usize> = archive
        .nodes()
        .iter()
        .enumerate()
        .filter_map(|(idx, _)| {
            let point = node_point(&archive, idx);
            (point.lon >= polygon[0].lon
                && point.lon <= polygon[1].lon
                && point.lat >= polygon[0].lat
                && point.lat <= polygon[2].lat)
                .then_some(idx)
        })
        .collect();

    let got: Vec<usize> = nodes_in_polygon(&archive, &polygon).collect();
    assert_eq!(got, expected);
    assert!(!got.is_empty());
    assert!(nodes_in_polygon(&archive, &polygon[..2]).next().is_none());
}
