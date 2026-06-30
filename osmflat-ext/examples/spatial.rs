//! Non-bbox spatial queries over an osmflat parent archive.
//!
//! These queries do not need an `Ext` sidecar; they use the parent archive's
//! existing spatial order and bbox query machinery.
//!
//! ```text
//! # nodes within 0.01 degrees of a point
//! cargo run --example spatial -- district-of-columbia.osmflat radius -77.0365 38.8977 0.01
//!
//! # 10 nearest nodes to a point
//! cargo run --example spatial -- district-of-columbia.osmflat nearest -77.0365 38.8977 --k 10
//!
//! # nodes inside a polygon, as lon/lat pairs
//! cargo run --example spatial -- district-of-columbia.osmflat polygon \
//!   -77.04 38.89 -77.01 38.89 -77.01 38.91 -77.04 38.91
//! ```
//!
//! LICENSE: the code in this example file is released into the Public Domain.

use clap::{Parser, Subcommand};
use osmflat::{find_tag, node_id, FileResourceStorage, Osm};
use osmflat_ext::spatial::{k_nearest_nodes, nodes_in_polygon, nodes_within_radius, Point};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Run non-bbox spatial node queries over an osmflat archive")]
struct Args {
    /// Parent osmflat archive directory.
    parent: PathBuf,
    #[command(subcommand)]
    command: Command,
    /// Maximum result rows to print.
    #[arg(long, default_value_t = 20, global = true)]
    limit: usize,
}

#[derive(Subcommand)]
enum Command {
    /// Nodes within radius degrees of a point.
    Radius {
        /// Longitude in degrees.
        #[arg(allow_hyphen_values = true)]
        lon: f64,
        /// Latitude in degrees.
        #[arg(allow_hyphen_values = true)]
        lat: f64,
        /// Radius in degrees.
        #[arg(allow_hyphen_values = true)]
        radius: f64,
    },
    /// K nearest nodes to a point.
    Nearest {
        /// Longitude in degrees.
        #[arg(allow_hyphen_values = true)]
        lon: f64,
        /// Latitude in degrees.
        #[arg(allow_hyphen_values = true)]
        lat: f64,
        /// Number of nearest nodes to return.
        #[arg(long, default_value_t = 10)]
        k: usize,
    },
    /// Nodes inside a polygon ring, supplied as lon/lat pairs.
    Polygon {
        /// Polygon coordinates as lon lat lon lat ...
        #[arg(allow_hyphen_values = true)]
        coords: Vec<f64>,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let archive = Osm::open(FileResourceStorage::new(args.parent))?;

    match args.command {
        Command::Radius { lon, lat, radius } => {
            let nodes: Vec<_> = nodes_within_radius(&archive, lon, lat, radius).collect();
            println!(
                "{} node(s) within radius {} of ({}, {})",
                nodes.len(),
                radius,
                lon,
                lat
            );
            print_nodes(&archive, &nodes, args.limit);
        }
        Command::Nearest { lon, lat, k } => {
            let nodes = k_nearest_nodes(&archive, lon, lat, k);
            println!("{} nearest node(s) to ({}, {})", nodes.len(), lon, lat);
            print_nodes(&archive, &nodes, args.limit);
        }
        Command::Polygon { coords } => {
            let polygon = parse_polygon(&coords)?;
            let nodes: Vec<_> = nodes_in_polygon(&archive, &polygon).collect();
            println!(
                "{} node(s) inside polygon with {} vertices",
                nodes.len(),
                polygon.len()
            );
            print_nodes(&archive, &nodes, args.limit);
        }
    }

    Ok(())
}

fn parse_polygon(coords: &[f64]) -> Result<Vec<Point>, Box<dyn std::error::Error>> {
    if coords.len() < 6 || coords.len() % 2 != 0 {
        return Err("polygon requires at least three lon/lat pairs".into());
    }

    Ok(coords
        .chunks_exact(2)
        .map(|pair| Point {
            lon: pair[0],
            lat: pair[1],
        })
        .collect())
}

fn print_nodes(parent: &Osm, nodes: &[usize], limit: usize) {
    for &idx in nodes.iter().take(limit) {
        let node = &parent.nodes()[idx];
        let scale = parent.header().coord_scale() as f64;
        let id = node_id(parent, idx).map_or_else(|| "?".to_string(), |id| id.to_string());
        println!(
            "  node {:>12}  idx={:<8} lon={:<12.7} lat={:<12.7} {}",
            id,
            idx,
            node.lon() as f64 / scale,
            node.lat() as f64 / scale,
            describe(parent, node.tags()),
        );
    }
    if nodes.len() > limit {
        println!("  ... (+{})", nodes.len() - limit);
    }
}

fn describe(parent: &Osm, tags: std::ops::Range<u64>) -> String {
    let pick = |key: &[u8]| {
        find_tag(parent, tags.clone(), key).map(|v| String::from_utf8_lossy(v).into_owned())
    };
    if let Some(name) = pick(b"name") {
        return format!("name={name}");
    }
    for key in [b"amenity".as_slice(), b"highway", b"shop", b"tourism"] {
        if let Some(value) = pick(key) {
            return format!("{}={value}", String::from_utf8_lossy(key));
        }
    }
    "(no descriptive tags)".to_string()
}
