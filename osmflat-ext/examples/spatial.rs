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
//!
//! # the same queries for ways or relations ("any part touches")
//! cargo run --example spatial -- district-of-columbia.osmflat --entity way nearest -77.0365 38.8977 --k 5
//! cargo run --example spatial -- district-of-columbia.osmflat --entity relation radius -77.0365 38.8977 0.002
//! ```
//!
//! LICENSE: the code in this example file is released into the Public Domain.

use clap::{Parser, Subcommand, ValueEnum};
use osmflat::{find_tag, node_id, relation_id, way_id, FileResourceStorage, Osm};
use osmflat_ext::spatial::{
    k_nearest_nodes, k_nearest_relations, k_nearest_ways, nodes_in_polygon, nodes_within_radius,
    relations_in_polygon, relations_within_radius, ways_in_polygon, ways_within_radius, Point,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Run non-bbox spatial queries over an osmflat archive")]
struct Args {
    /// Parent osmflat archive directory.
    parent: PathBuf,
    #[command(subcommand)]
    command: Command,
    /// Maximum result rows to print.
    #[arg(long, default_value_t = 20, global = true)]
    limit: usize,
    /// Entity type to query. Ways and relations match when any part touches.
    #[arg(long, value_enum, default_value_t = Entity::Node, global = true)]
    entity: Entity,
}

#[derive(Clone, Copy, ValueEnum)]
enum Entity {
    Node,
    Way,
    Relation,
}

impl Entity {
    fn plural(self) -> &'static str {
        match self {
            Entity::Node => "node(s)",
            Entity::Way => "way(s)",
            Entity::Relation => "relation(s)",
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Entities within radius degrees of a point, nearest-first.
    #[command(allow_negative_numbers = true)]
    Radius {
        /// Longitude in degrees.
        lon: f64,
        /// Latitude in degrees.
        lat: f64,
        /// Radius in degrees.
        radius: f64,
    },
    /// K nearest entities to a point.
    #[command(allow_negative_numbers = true)]
    Nearest {
        /// Longitude in degrees.
        lon: f64,
        /// Latitude in degrees.
        lat: f64,
        /// Number of nearest entities to return.
        #[arg(long, default_value_t = 10)]
        k: usize,
    },
    /// Entities inside (or, for ways and relations, touching) a polygon ring, as lon/lat pairs.
    #[command(allow_negative_numbers = true)]
    Polygon {
        /// Polygon coordinates as lon lat lon lat ...
        coords: Vec<f64>,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    let archive = Osm::open(FileResourceStorage::new(args.parent))?;

    let entity = args.entity;
    let found: Vec<usize> = match args.command {
        Command::Radius { lon, lat, radius } => {
            let found: Vec<_> = match entity {
                Entity::Node => nodes_within_radius(&archive, lon, lat, radius).collect(),
                Entity::Way => ways_within_radius(&archive, lon, lat, radius).collect(),
                Entity::Relation => relations_within_radius(&archive, lon, lat, radius).collect(),
            };
            println!(
                "{} {} within radius {} of ({}, {})",
                found.len(),
                entity.plural(),
                radius,
                lon,
                lat
            );
            found
        }
        Command::Nearest { lon, lat, k } => {
            let found = match entity {
                Entity::Node => k_nearest_nodes(&archive, lon, lat, k),
                Entity::Way => k_nearest_ways(&archive, lon, lat, k),
                Entity::Relation => k_nearest_relations(&archive, lon, lat, k),
            };
            println!(
                "{} nearest {} to ({}, {})",
                found.len(),
                entity.plural(),
                lon,
                lat
            );
            found
        }
        Command::Polygon { coords } => {
            let polygon = parse_polygon(&coords)?;
            let found: Vec<_> = match entity {
                Entity::Node => nodes_in_polygon(&archive, &polygon).collect(),
                Entity::Way => ways_in_polygon(&archive, &polygon).collect(),
                Entity::Relation => relations_in_polygon(&archive, &polygon).collect(),
            };
            println!(
                "{} {} inside polygon with {} vertices",
                found.len(),
                entity.plural(),
                polygon.len()
            );
            found
        }
    };
    match entity {
        Entity::Node => print_nodes(&archive, &found, args.limit),
        Entity::Way | Entity::Relation => print_entities(&archive, entity, &found, args.limit),
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

fn print_entities(parent: &Osm, entity: Entity, found: &[usize], limit: usize) {
    for &idx in found.iter().take(limit) {
        let (label, id, tags) = match entity {
            Entity::Way => ("way", way_id(parent, idx), parent.ways()[idx].tags()),
            _ => (
                "relation",
                relation_id(parent, idx),
                parent.relations()[idx].tags(),
            ),
        };
        let id = id.map_or_else(|| "?".to_string(), |id| id.to_string());
        println!(
            "  {label:<8} {id:>12}  idx={idx:<10} {}",
            describe(parent, tags)
        );
    }
    if found.len() > limit {
        println!("  ... (+{})", found.len() - limit);
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
