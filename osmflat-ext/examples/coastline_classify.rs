//! Classify a list of (lon, lat) points as land/water via the ray-casting
//! test, against a real parent archive -- no sidecar needed.
//!
//! ```text
//! cargo run --release --example coastline_classify -- us.osm.flat -122.20 47.61 -122.35 47.60
//! ```
//!
//! LICENSE: the code in this example file is released into the Public Domain.

use osmflat::{FileResourceStorage, Osm};
use osmflat_ext::coastline::classify_points;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 || args.len() % 2 != 0 {
        eprintln!("usage: coastline_classify <archive> <lon1> <lat1> [<lon2> <lat2> ...]");
        std::process::exit(2);
    }
    let archive_path = PathBuf::from(&args[1]);
    let points: Vec<(f64, f64)> = args[2..]
        .chunks(2)
        .map(|c| (c[0].parse().unwrap(), c[1].parse().unwrap()))
        .collect();

    let archive = Osm::open(FileResourceStorage::new(archive_path)).expect("open archive");
    let scale = archive.header().coord_scale() as f64;

    eprintln!(
        "classifying {} point(s) (one pass over the archive)...",
        points.len()
    );
    let results = classify_points(&archive, scale, &points);
    for ((lon, lat), is_land) in points.iter().zip(results.iter()) {
        println!(
            "({lon:.4},{lat:.4}) -> {}",
            if *is_land { "LAND" } else { "WATER" }
        );
    }
}
