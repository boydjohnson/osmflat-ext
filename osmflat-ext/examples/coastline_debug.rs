//! Debug helper: run `assemble_coastline` against a real parent archive and
//! list the rings whose bounding box overlaps a given lon/lat window, with
//! their land/water classification, area, and vertex count. Useful for
//! tracking down a ring that failed to assemble or got misclassified,
//! without waiting on a full `osmflat-extc --coastline` build.
//!
//! ```text
//! cargo run --release --example coastline_debug -- us.osm.flat -122.48 47.45 -122.15 47.78
//! ```
//!
//! LICENSE: the code in this example file is released into the Public Domain.

use osmflat::{FileResourceStorage, Osm};
use osmflat_ext::coastline::assemble_coastline_raw;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 6 {
        eprintln!("usage: coastline_debug <archive> <min_lon> <min_lat> <max_lon> <max_lat>");
        std::process::exit(2);
    }
    let archive_path = PathBuf::from(&args[1]);
    let min_lon: f64 = args[2].parse().unwrap();
    let min_lat: f64 = args[3].parse().unwrap();
    let max_lon: f64 = args[4].parse().unwrap();
    let max_lat: f64 = args[5].parse().unwrap();

    let archive = Osm::open(FileResourceStorage::new(archive_path)).expect("open archive");
    let scale = archive.header().coord_scale() as f64;

    let h = archive.header();
    println!(
        "archive bbox: lon=[{:.4},{:.4}] lat=[{:.4},{:.4}]",
        h.bbox_left() as f64 / scale,
        h.bbox_right() as f64 / scale,
        h.bbox_bottom() as f64 / scale,
        h.bbox_top() as f64 / scale,
    );

    eprintln!("assembling coastline (whole archive, no bbox filter on input)...");
    let (rings, open_chains) = assemble_coastline_raw(&archive, scale);
    eprintln!(
        "assembled {} closed rings, {} open (unclosed) chains total",
        rings.len(),
        open_chains.len()
    );

    println!("--- open chain endpoints overlapping bbox ---");
    let mut shown_chains = 0;
    for chain in &open_chains {
        let first = chain.first().unwrap();
        let last = chain.last().unwrap();
        let touches = |v: &osmflat_ext::multipolygon::Vertex| {
            v.lon >= min_lon && v.lon <= max_lon && v.lat >= min_lat && v.lat <= max_lat
        };
        if !touches(first) && !touches(last) {
            continue;
        }
        shown_chains += 1;
        println!(
            "chain n_vertices={} start=({:.4},{:.4}) end=({:.4},{:.4})",
            chain.len(),
            first.lon,
            first.lat,
            last.lon,
            last.lat
        );
    }
    println!("{shown_chains} open chain(s) with an endpoint in the given bbox");

    let mut shown = 0;
    for ring in &rings {
        let (mut r_min_lon, mut r_min_lat) = (f64::INFINITY, f64::INFINITY);
        let (mut r_max_lon, mut r_max_lat) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for v in &ring.vertices {
            r_min_lon = r_min_lon.min(v.lon);
            r_min_lat = r_min_lat.min(v.lat);
            r_max_lon = r_max_lon.max(v.lon);
            r_max_lat = r_max_lat.max(v.lat);
        }
        let overlaps = r_min_lon <= max_lon
            && r_max_lon >= min_lon
            && r_min_lat <= max_lat
            && r_max_lat >= min_lat;
        if !overlaps {
            continue;
        }
        shown += 1;
        println!(
            "is_land={} area_m2={:.0} n_vertices={} bbox=[{:.4},{:.4},{:.4},{:.4}]",
            ring.is_land,
            ring.area_m2,
            ring.vertices.len(),
            r_min_lon,
            r_min_lat,
            r_max_lon,
            r_max_lat
        );
    }
    eprintln!("{shown} ring(s) overlap the given bbox");

    println!("--- top 10 largest rings overall ---");
    for ring in rings.iter().take(10) {
        let (mut r_min_lon, mut r_min_lat) = (f64::INFINITY, f64::INFINITY);
        let (mut r_max_lon, mut r_max_lat) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for v in &ring.vertices {
            r_min_lon = r_min_lon.min(v.lon);
            r_min_lat = r_min_lat.min(v.lat);
            r_max_lon = r_max_lon.max(v.lon);
            r_max_lat = r_max_lat.max(v.lat);
        }
        println!(
            "is_land={} area_m2={:.0} n_vertices={} bbox=[{:.4},{:.4},{:.4},{:.4}]",
            ring.is_land,
            ring.area_m2,
            ring.vertices.len(),
            r_min_lon,
            r_min_lat,
            r_max_lon,
            r_max_lat
        );
    }

    println!("--- top 10 largest open chains (by vertex count) ---");
    let mut sorted_chains: Vec<&Vec<osmflat_ext::multipolygon::Vertex>> =
        open_chains.iter().collect();
    sorted_chains.sort_by_key(|c| std::cmp::Reverse(c.len()));
    for chain in sorted_chains.iter().take(10) {
        let first = chain.first().unwrap();
        let last = chain.last().unwrap();
        println!(
            "n_vertices={} start=({:.4},{:.4}) end=({:.4},{:.4})",
            chain.len(),
            first.lon,
            first.lat,
            last.lon,
            last.lat
        );
    }
}
