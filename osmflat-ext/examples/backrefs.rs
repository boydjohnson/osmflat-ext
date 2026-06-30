//! Reverse-reference lookups over an `Ext` sidecar: which ways use a node, and
//! which relations contain a node / way / relation.
//!
//! Build the sidecar first (the parent needs ids for the OSM-id lookup, i.e.
//! `osmflatc --reverse-ids`):
//!
//! ```text
//! osmflat-extc --backrefs --out planet.osm.ext planet.osm.flatdata
//! ```
//!
//! Then look up by OSM id:
//!
//! ```text
//! # ways that use node 123, and relations containing it
//! cargo run --example backrefs -- planet.osm.flatdata planet.osm.ext node 123
//!
//! # relations containing way 456
//! cargo run --example backrefs -- planet.osm.flatdata planet.osm.ext way 456
//!
//! # relations containing relation 789
//! cargo run --example backrefs -- planet.osm.flatdata planet.osm.ext relation 789
//! ```
//!
//! LICENSE: the code in this example file is released into the Public Domain.

use clap::{Parser, ValueEnum};
use osmflat::{
    find_tag, node_idx_by_id, relation_id, relation_idx_by_id, way_id, way_idx_by_id,
    FileResourceStorage, Osm,
};
use osmflat_ext::backrefs::BackrefsQuery;
use osmflat_ext::{Ext, ExtArchive, Ref};
use std::path::PathBuf;

#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    Node,
    Way,
    Relation,
}

#[derive(Parser)]
#[command(about = "Reverse-reference lookups over an osmflat-ext sidecar")]
struct Args {
    /// Parent osmflat archive directory (built with `osmflatc --reverse-ids`).
    parent: PathBuf,
    /// Sibling Ext archive directory (built with `--backrefs`).
    ext: PathBuf,
    /// Kind of entity the OSM id refers to.
    kind: Kind,
    /// OSM id of the entity to look up.
    id: u64,
    /// Max parents to list per relationship.
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let parent = Osm::open(FileResourceStorage::new(args.parent))?;
    let ext = Ext::open(FileResourceStorage::new(args.ext))?;
    let archive = ExtArchive::open(parent, ext)?;
    let br = archive
        .backrefs()
        .ok_or("sidecar has no backrefs sub-archive (rebuild with --backrefs)")?;
    let parent = archive.parent();

    match args.kind {
        Kind::Node => {
            let idx = node_idx_by_id(parent, args.id)
                .ok_or_else(|| format!("node {} not in archive (or no reverse ids)", args.id))?;
            println!("node {} -> index {idx}", args.id);
            print_ways(parent, &br, idx, args.limit);
            print_relations(parent, br.relations_with_node(idx), args.limit);
        }
        Kind::Way => {
            let idx = way_idx_by_id(parent, args.id)
                .ok_or_else(|| format!("way {} not in archive (or no reverse ids)", args.id))?;
            println!("way {} -> index {idx}", args.id);
            print_relations(parent, br.relations_with_way(idx), args.limit);
        }
        Kind::Relation => {
            let idx = relation_idx_by_id(parent, args.id).ok_or_else(|| {
                format!("relation {} not in archive (or no reverse ids)", args.id)
            })?;
            println!("relation {} -> index {idx}", args.id);
            print_relations(parent, br.relations_with_relation(idx), args.limit);
        }
    }
    Ok(())
}

fn print_ways(parent: &Osm, br: &BackrefsQuery, node_idx: usize, limit: usize) {
    let ways = br.ways_using_node(node_idx);
    println!("\nused by {} way(s):", ways.len());
    for r in ways.iter().take(limit) {
        let idx = r.value() as usize;
        let way = &parent.ways()[idx];
        println!(
            "  way {:>12}  {}",
            way_id(parent, idx).map_or_else(|| "?".to_string(), |id| id.to_string()),
            describe(parent, way.tags()),
        );
    }
    if ways.len() > limit {
        println!("  … (+{})", ways.len() - limit);
    }
}

fn print_relations(parent: &Osm, rels: &[Ref], limit: usize) {
    println!("\ncontained in {} relation(s):", rels.len());
    for r in rels.iter().take(limit) {
        let idx = r.value() as usize;
        let relation = &parent.relations()[idx];
        println!(
            "  relation {:>12}  {}",
            relation_id(parent, idx).map_or_else(|| "?".to_string(), |id| id.to_string()),
            describe(parent, relation.tags()),
        );
    }
    if rels.len() > limit {
        println!("  … (+{})", rels.len() - limit);
    }
}

/// A short, human-friendly label for an entity from its tags.
fn describe(parent: &Osm, tags: std::ops::Range<u64>) -> String {
    let pick = |key: &[u8]| {
        find_tag(parent, tags.clone(), key).map(|v| String::from_utf8_lossy(v).into_owned())
    };
    if let Some(name) = pick(b"name") {
        return format!("name={name}");
    }
    for key in [b"highway".as_slice(), b"building", b"type", b"amenity"] {
        if let Some(value) = pick(key) {
            return format!("{}={value}", String::from_utf8_lossy(key));
        }
    }
    "(no descriptive tags)".to_string()
}
