#![cfg(feature = "test-support")]

use osmflat::test_support::{build_archive, scale};
use osmflat::Osm;
use osmflat_ext::spatial::{k_nearest_nodes, nodes_in_polygon, nodes_within_radius, ScaledPoint};

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

fn point(lon: f64, lat: f64) -> ScaledPoint {
    ScaledPoint {
        lon: scale(lon),
        lat: scale(lat),
    }
}

fn distance_sq(a: ScaledPoint, b: ScaledPoint) -> i128 {
    let dx = a.lon as i128 - b.lon as i128;
    let dy = a.lat as i128 - b.lat as i128;
    dx * dx + dy * dy
}

fn node_point(archive: &Osm, idx: usize) -> ScaledPoint {
    let node = &archive.nodes()[idx];
    ScaledPoint {
        lon: node.lon(),
        lat: node.lat(),
    }
}

#[test]
fn nodes_within_radius_matches_exact_scan_nearest_first() {
    let archive = archive();
    let center = point(-77.025, 38.902);
    let radius = scale(0.011) as i64;
    let radius_sq = (radius as i128) * (radius as i128);

    let mut expected: Vec<(i128, usize)> = archive
        .nodes()
        .iter()
        .enumerate()
        .filter_map(|(idx, _)| {
            let dist = distance_sq(center, node_point(&archive, idx));
            (dist <= radius_sq).then_some((dist, idx))
        })
        .collect();
    expected.sort_by_key(|&(dist, idx)| (dist, idx));
    let expected: Vec<usize> = expected.into_iter().map(|(_, idx)| idx).collect();

    let got: Vec<usize> = nodes_within_radius(&archive, center, radius).collect();
    assert_eq!(got, expected);
    assert!(nodes_within_radius(&archive, center, -1).next().is_none());
}

#[test]
fn k_nearest_nodes_matches_exact_scan() {
    let archive = archive();
    let center = point(-77.024, 38.901);

    let mut expected: Vec<(i128, usize)> = archive
        .nodes()
        .iter()
        .enumerate()
        .map(|(idx, _)| (distance_sq(center, node_point(&archive, idx)), idx))
        .collect();
    expected.sort_by_key(|&(dist, idx)| (dist, idx));

    assert_eq!(k_nearest_nodes(&archive, center, 0), Vec::<usize>::new());
    assert_eq!(
        k_nearest_nodes(&archive, center, 3),
        expected
            .iter()
            .take(3)
            .map(|&(_, idx)| idx)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        k_nearest_nodes(&archive, center, archive.nodes().len() + 10),
        expected.iter().map(|&(_, idx)| idx).collect::<Vec<_>>()
    );
}

#[test]
fn nodes_in_polygon_matches_rectangle_scan_and_includes_boundary() {
    let archive = archive();
    let polygon = [
        point(-77.030, 38.895),
        point(-77.015, 38.895),
        point(-77.015, 38.905),
        point(-77.030, 38.905),
    ];

    let expected: Vec<usize> = archive
        .nodes()
        .iter()
        .enumerate()
        .filter_map(|(idx, node)| {
            (node.lon() >= polygon[0].lon
                && node.lon() <= polygon[1].lon
                && node.lat() >= polygon[0].lat
                && node.lat() <= polygon[2].lat)
                .then_some(idx)
        })
        .collect();

    let got: Vec<usize> = nodes_in_polygon(&archive, &polygon).collect();
    assert_eq!(got, expected);
    assert!(!got.is_empty());
    assert!(nodes_in_polygon(&archive, &polygon[..2]).next().is_none());
}
