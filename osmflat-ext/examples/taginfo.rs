//! Browse the inverted tag index / taginfo histograms of an `Ext` sidecar,
//! the way <https://taginfo.openstreetmap.org> presents a database.
//!
//! Build the sidecar first:
//!
//! ```text
//! osmflat-extc --taginfo --out planet.osm.ext planet.osm.flatdata
//! ```
//!
//! Then:
//!
//! ```text
//! # the top keys, by object count (the "keys" table)
//! cargo run --example taginfo -- planet.osm.flatdata planet.osm.ext
//!
//! # one key: its stats and most common values (the "values" table)
//! cargo run --example taginfo -- planet.osm.flatdata planet.osm.ext highway
//!
//! # one key=value: counts by type and a few example OSM ids
//! cargo run --example taginfo -- planet.osm.flatdata planet.osm.ext highway primary
//! ```
//!
//! LICENSE: the code in this example file is released into the Public Domain.

use clap::Parser;
use osmflat::{node_id, relation_id, way_id, FileResourceStorage, Osm};
use osmflat_ext::taginfo::{TaginfoQuery, TypeCounts};
use osmflat_ext::{Ext, ExtArchive};
use std::path::PathBuf;

#[derive(Parser)]
#[command(about = "Browse the taginfo index of an osmflat-ext sidecar")]
struct Args {
    /// Parent osmflat archive directory.
    parent: PathBuf,
    /// Sibling Ext archive directory (built with `--taginfo`).
    ext: PathBuf,
    /// Optional key to inspect; omit to list the top keys.
    key: Option<String>,
    /// Optional value (with `key`) to inspect a single `key=value`.
    value: Option<String>,
    /// How many rows to print.
    #[arg(long, default_value_t = 20)]
    top: usize,
}

fn total(c: TypeCounts) -> u64 {
    c.nodes + c.ways + c.relations
}

fn s(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    String::from_utf8_lossy(bytes)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let parent = Osm::open(FileResourceStorage::new(args.parent))?;
    let ext = Ext::open(FileResourceStorage::new(args.ext))?;
    let archive = ExtArchive::open(parent, ext)?;
    let tq = archive
        .taginfo()
        .ok_or("sidecar has no taginfo sub-archive (rebuild with --taginfo)")?;

    match (args.key.as_deref(), args.value.as_deref()) {
        (None, _) => list_keys(&tq, args.top),
        (Some(key), None) => show_key(&tq, key, args.top),
        (Some(key), Some(value)) => show_kv(archive.parent(), &tq, key, value, args.top),
    }
}

/// The "keys" table: distinct keys ranked by object count.
fn list_keys(tq: &TaginfoQuery, top: usize) -> Result<(), Box<dyn std::error::Error>> {
    let mut keys: Vec<_> = tq
        .keys()
        .map(|k| (k.key().to_vec(), k.counts(), k.distinct_values()))
        .collect();
    keys.sort_by_key(|(_, c, _)| std::cmp::Reverse(total(*c)));

    println!(
        "{:<28} {:>12} {:>10} {:>10} {:>9}",
        "key", "objects", "nodes", "ways", "values"
    );
    for (key, c, values) in keys.into_iter().take(top) {
        println!(
            "{:<28} {:>12} {:>10} {:>10} {:>9}",
            s(&key),
            total(c),
            c.nodes,
            c.ways,
            values,
        );
    }
    Ok(())
}

/// The "values" table for one key.
fn show_key(tq: &TaginfoQuery, key: &str, top: usize) -> Result<(), Box<dyn std::error::Error>> {
    let k = tq
        .key(key.as_bytes())
        .ok_or_else(|| format!("key {key:?} not found"))?;
    let c = k.counts();
    println!(
        "{key}: {} objects ({} nodes, {} ways, {} relations), {} distinct values\n",
        total(c),
        c.nodes,
        c.ways,
        c.relations,
        k.distinct_values(),
    );

    let mut values: Vec<_> = k
        .values()
        .map(|v| (v.value().to_vec(), v.counts()))
        .collect();
    values.sort_by_key(|(_, c)| std::cmp::Reverse(total(*c)));

    println!(
        "{:<28} {:>12} {:>10} {:>10} {:>9}",
        "value", "objects", "nodes", "ways", "rels"
    );
    for (value, c) in values.into_iter().take(top) {
        println!(
            "{:<28} {:>12} {:>10} {:>10} {:>9}",
            s(&value),
            total(c),
            c.nodes,
            c.ways,
            c.relations,
        );
    }
    Ok(())
}

/// One `key=value`: counts and a few example OSM ids per type.
fn show_kv(
    parent: &Osm,
    tq: &TaginfoQuery,
    key: &str,
    value: &str,
    examples: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let v = tq
        .kv(key.as_bytes(), value.as_bytes())
        .ok_or_else(|| format!("{key}={value} not found"))?;
    let c = v.counts();
    println!(
        "{key}={value}: {} objects ({} nodes, {} ways, {} relations)\n",
        total(c),
        c.nodes,
        c.ways,
        c.relations,
    );

    print_ids("nodes", v.nodes(), examples, |i| node_id(parent, i));
    print_ids("ways", v.ways(), examples, |i| way_id(parent, i));
    print_ids("relations", v.relations(), examples, |i| {
        relation_id(parent, i)
    });
    Ok(())
}

fn print_ids(
    label: &str,
    postings: &[osmflat_ext::Ref],
    limit: usize,
    id_of: impl Fn(usize) -> Option<u64>,
) {
    if postings.is_empty() {
        return;
    }
    let ids: Vec<String> = postings
        .iter()
        .take(limit)
        .map(|r| id_of(r.value() as usize).map_or_else(|| "?".into(), |id| id.to_string()))
        .collect();
    let more = postings.len().saturating_sub(limit);
    let suffix = if more > 0 {
        format!(", … (+{more})")
    } else {
        String::new()
    };
    println!("  example {label}: {}{suffix}", ids.join(", "));
}
