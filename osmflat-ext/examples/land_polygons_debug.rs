//! Inspect a `LandPolygons` sidecar built with `osmflat-extc --land-polygons`:
//! total ring count, and how many rings' own bbox overlaps a query bbox.
//!
//! ```text
//! cargo run --release --example land_polygons_debug -- us.osm.flat us.osm.ext -122.48 47.45 -122.15 47.78
//! ```
//!
//! LICENSE: the code in this example file is released into the Public Domain.

use osmflat::{FileResourceStorage, Osm};
use osmflat_ext::{Ext, ExtArchive};
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 7 {
        eprintln!("usage: land_polygons_debug <archive> <ext> <minlon> <minlat> <maxlon> <maxlat>");
        std::process::exit(2);
    }
    let archive_path = PathBuf::from(&args[1]);
    let ext_path = PathBuf::from(&args[2]);
    let min_lon: f64 = args[3].parse().unwrap();
    let min_lat: f64 = args[4].parse().unwrap();
    let max_lon: f64 = args[5].parse().unwrap();
    let max_lat: f64 = args[6].parse().unwrap();

    let parent = Osm::open(FileResourceStorage::new(archive_path)).expect("open parent");
    let ext = Ext::open(FileResourceStorage::new(ext_path)).expect("open ext");
    let archive = ExtArchive::open(parent, ext).expect("fingerprint mismatch");

    let land_polygons = archive
        .land_polygons()
        .expect("sidecar built without --land-polygons");

    let rings = land_polygons.rings();
    eprintln!("{} total ring(s) in LandPolygons sidecar", rings.len());

    let mut land_count = 0;
    let mut hole_count = 0;
    let mut overlapping = 0;
    for ring in &rings {
        if ring.is_land {
            land_count += 1;
        } else {
            hole_count += 1;
        }
        let (mut vmin_lon, mut vmin_lat) = (f64::INFINITY, f64::INFINITY);
        let (mut vmax_lon, mut vmax_lat) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &(lon, lat) in &ring.vertices {
            vmin_lon = vmin_lon.min(lon);
            vmax_lon = vmax_lon.max(lon);
            vmin_lat = vmin_lat.min(lat);
            vmax_lat = vmax_lat.max(lat);
        }
        let overlaps = vmin_lon <= max_lon
            && vmax_lon >= min_lon
            && vmin_lat <= max_lat
            && vmax_lat >= min_lat;
        if overlaps {
            overlapping += 1;
            if overlapping <= 5 {
                eprintln!(
                    "  overlap: is_land={} bbox=({vmin_lon:.4},{vmin_lat:.4})-({vmax_lon:.4},{vmax_lat:.4}) n={}",
                    ring.is_land,
                    ring.vertices.len()
                );
            }
        }
    }
    eprintln!("land rings: {land_count}, hole rings: {hole_count}");
    eprintln!("rings overlapping query bbox: {overlapping}");
}
