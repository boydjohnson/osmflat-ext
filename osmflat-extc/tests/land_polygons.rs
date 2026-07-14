#![cfg(feature = "test-support")]

//! End-to-end check for `--land-polygons`: write a tiny synthetic shapefile
//! (Web Mercator/EPSG:3857, mirroring the real osmdata.openstreetmap.de
//! format) with an outer land ring, a hole punched in it, and a ring far
//! outside the parent archive's bbox, then confirm the importer reprojects to
//! WGS84, filters out the far-away ring, and classifies land/hole from the
//! shapefile's own `Outer`/`Inner` ring role -- not from the sign of the
//! ring's own area, which the shapefile crate's canonical ESRI winding
//! (outer clockwise, holes counter-clockwise) would get backwards relative to
//! `natural=coastline`'s "land on the left" convention.

use osmflat_extc::test_support::{
    build_ext_archive, build_parent_archive, Fixture, NodeSpec, COORD_SCALE,
};
use osmflat_extc::BuildOptions;
use shapefile::{Point, Polygon, PolygonRing};
use std::f64::consts::PI;

const WEB_MERCATOR_R: f64 = 6378137.0;

fn web_mercator_to_wgs84(x: f64, y: f64) -> (f64, f64) {
    let lon = x / WEB_MERCATOR_R * 180.0 / PI;
    let lat = (2.0 * (y / WEB_MERCATOR_R).exp().atan() - PI / 2.0) * 180.0 / PI;
    (lon, lat)
}

/// A single dummy node -- the fixture only needs to exist so the parent
/// archive has a valid header/coord_scale; its geometry is unrelated to the
/// shapefile being imported.
fn fixture() -> Fixture {
    Fixture {
        nodes: vec![NodeSpec {
            lon: 0.0,
            lat: 0.0,
            tags: vec![],
        }],
        ways: vec![],
        relations: vec![],
    }
}

#[test]
fn imports_and_reprojects_overlapping_rings_and_filters_far_away_ones() {
    let dir = tempfile::tempdir().unwrap();
    let shp_path = dir.path().join("test_land_polygons.shp");

    // Outer land ring straddling the Web Mercator origin, given in
    // counter-clockwise point order -- the shapefile crate canonicalizes
    // `Outer` rings to clockwise regardless of input order (it will reverse
    // this one), so this proves the importer takes land/hole from the ring's
    // declared role, not from re-deriving a sign off whatever order the
    // points happened to arrive in. Overlaps the parent archive's (unset, so
    // (0,0,0,0)) bbox.
    let land_ring = PolygonRing::Outer(vec![
        Point::new(-1000.0, -1000.0),
        Point::new(1000.0, -1000.0),
        Point::new(1000.0, 1000.0),
        Point::new(-1000.0, 1000.0),
        Point::new(-1000.0, -1000.0),
    ]);
    // A hole punched in the land ring -- must come out classified as not-land.
    let hole_ring = PolygonRing::Inner(vec![
        Point::new(-100.0, -100.0),
        Point::new(100.0, -100.0),
        Point::new(100.0, 100.0),
        Point::new(-100.0, 100.0),
        Point::new(-100.0, -100.0),
    ]);
    // Small square far out in the positive quadrant -- doesn't overlap the
    // (0,0,0,0) bbox and should be filtered out entirely.
    let far_ring = PolygonRing::Outer(vec![
        Point::new(1_999_000.0, 1_999_000.0),
        Point::new(2_001_000.0, 1_999_000.0),
        Point::new(2_001_000.0, 2_001_000.0),
        Point::new(1_999_000.0, 2_001_000.0),
        Point::new(1_999_000.0, 1_999_000.0),
    ]);

    {
        let mut writer = shapefile::ShapeWriter::from_path(&shp_path).unwrap();
        writer
            .write_shape(&Polygon::with_rings(vec![land_ring, hole_ring]))
            .unwrap();
        writer.write_shape(&Polygon::new(far_ring)).unwrap();
    }

    let parent = build_parent_archive(&fixture()).unwrap();
    let opts = BuildOptions {
        land_polygons: Some(shp_path),
        ..Default::default()
    };
    let ext_archive = build_ext_archive(parent, &opts).unwrap();
    let land_polygons = ext_archive
        .land_polygons()
        .expect("built with --land-polygons");
    let rings = land_polygons.rings();

    assert_eq!(
        rings.len(),
        2,
        "the far-away ring must be filtered out, land+hole kept"
    );
    // Sorted by area descending: the big land ring first, the small hole second.
    assert!(rings[0].is_land, "outer ring must be classified as land");
    assert!(!rings[1].is_land, "inner ring must be classified as a hole");

    // Compare as a set of corners, not an exact sequence: the shapefile crate
    // may reverse and/or rotate the ring's starting point when canonicalizing
    // winding, and that reordering is an internal implementation detail we
    // don't need to (and shouldn't) assert against.
    let mut expected: Vec<(f64, f64)> = [
        (-1000.0, -1000.0),
        (1000.0, -1000.0),
        (1000.0, 1000.0),
        (-1000.0, 1000.0),
    ]
    .iter()
    .map(|&(x, y)| web_mercator_to_wgs84(x, y))
    .collect();
    expected.sort_by(|a, b| a.partial_cmp(b).unwrap());

    // Drop the closing duplicate vertex before comparing corners.
    let mut actual: Vec<(f64, f64)> = rings[0].vertices[..rings[0].vertices.len() - 1].to_vec();
    actual.sort_by(|a, b| a.partial_cmp(b).unwrap());

    assert_eq!(actual.len(), expected.len());
    for (&(lon, lat), &(elon, elat)) in actual.iter().zip(expected.iter()) {
        assert!((lon - elon).abs() < 1.0 / COORD_SCALE as f64);
        assert!((lat - elat).abs() < 1.0 / COORD_SCALE as f64);
    }
}
