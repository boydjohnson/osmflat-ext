//! List every coastline segment crossing an eastward ray from a given point,
//! in order, with each one's contribution -- for debugging a surprising
//! `is_land`/`classify_points` result.
//!
//! ```text
//! cargo run --release --example coastline_ray_debug -- us.osm.flat -122.20 47.61
//! ```
//!
//! LICENSE: the code in this example file is released into the Public Domain.

use osmflat::{find_tag, FileResourceStorage, Osm};
use osmflat_ext::multipolygon::way_node_indices;
use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        eprintln!("usage: coastline_ray_debug <archive> <lon> <lat>");
        std::process::exit(2);
    }
    let archive =
        Osm::open(FileResourceStorage::new(PathBuf::from(&args[1]))).expect("open archive");
    let lon: f64 = args[2].parse().unwrap();
    let lat: f64 = args[3].parse().unwrap();
    let scale = archive.header().coord_scale() as f64;
    let nodes = archive.nodes();

    let mut crossings: Vec<(f64, i64, u64)> = Vec::new(); // (crossing_lon, contribution, way real osm_id-ish idx)
    let mut total = 0i64;

    for (way_idx, way) in archive.ways().iter().enumerate() {
        if find_tag(&archive, way.tags(), b"natural") != Some(b"coastline") {
            continue;
        }
        let idx = way_node_indices(&archive, &way);
        let resolve = |n: u64| -> (f64, f64) {
            let node = &nodes[n as usize];
            (node.lon() as f64 / scale, node.lat() as f64 / scale)
        };
        for w in idx.windows(2) {
            let (a, b) = (resolve(w[0]), resolve(w[1]));
            let (upward, lo, hi) = if a.1 < b.1 {
                (true, a, b)
            } else {
                (false, b, a)
            };
            if lo.1 == hi.1 || !(lo.1 <= lat && lat < hi.1) {
                continue;
            }
            let t = (lat - lo.1) / (hi.1 - lo.1);
            let crossing_lon = lo.0 + t * (hi.0 - lo.0);
            if crossing_lon <= lon {
                continue;
            }
            let contribution = if upward { 1 } else { -1 };
            total += contribution;
            crossings.push((crossing_lon, contribution, way_idx as u64));
        }
    }
    crossings.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    for (crossing_lon, contribution, way_idx) in &crossings {
        println!("crossing_lon={crossing_lon:.4} contribution={contribution:+} way_idx={way_idx}");
    }
    println!("total crossings east of point: {}", crossings.len());
    println!(
        "net winding: {total} -> {}",
        if total > 0 { "LAND" } else { "WATER" }
    );
}
