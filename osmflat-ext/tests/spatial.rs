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
