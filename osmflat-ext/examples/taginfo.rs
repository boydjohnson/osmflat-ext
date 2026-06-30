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
//! cargo run --example taginfo -- planet.osm.flatdata planet.osm.ext highway --combinations
//!
//! # one key=value: counts by type and a few example OSM ids
//! cargo run --example taginfo -- planet.osm.flatdata planet.osm.ext highway primary
//! cargo run --example taginfo -- planet.osm.flatdata planet.osm.ext highway primary --combinations
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
    /// With a key, print co-occurring keys; with key=value, print co-occurring tags.
    ///
    /// Requires building the sidecar with `osmflat-extc --combinations`.
    #[arg(long)]
    combinations: bool,
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

    match (
        args.key.as_deref(),
        args.value.as_deref(),
        args.combinations,
    ) {
        (None, _, _) => list_keys(&tq, args.top),
        (Some(key), None, false) => show_key(&tq, key, args.top),
        (Some(key), None, true) => show_key_combinations(&tq, key, args.top),
        (Some(key), Some(value), true) => show_tag_combinations(&tq, key, value, args.top),
        (Some(key), Some(value), false) => show_kv(archive.parent(), &tq, key, value, args.top),
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

/// Taginfo "combinations": other keys used by objects that carry this key.
fn show_key_combinations(
    tq: &TaginfoQuery,
    key: &str,
    top: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let k = tq
        .key(key.as_bytes())
        .ok_or_else(|| format!("key {key:?} not found"))?;
    let combos: Vec<_> = k
        .combinations()
        .map(|c| (c.key().to_vec(), c.together_count()))
        .collect();

    if combos.is_empty() {
        println!(
            "{key}: no combinations stored (rebuild the sidecar with osmflat-extc --combinations)"
        );
        return Ok(());
    }

    println!("{key}: top co-occurring keys\n");
    println!("{:<28} {:>12}", "other key", "together");
    for (other_key, together_count) in combos.into_iter().take(top) {
        println!("{:<28} {:>12}", s(&other_key), together_count);
    }

    Ok(())
}

/// Taginfo "combinations" for one `key=value`: other tags used by matching objects.
fn show_tag_combinations(
    tq: &TaginfoQuery,
    key: &str,
    value: &str,
    top: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let tag = tq
        .kv(key.as_bytes(), value.as_bytes())
        .ok_or_else(|| format!("{key}={value} not found"))?;
    let combos: Vec<_> = tag
        .combinations()
        .map(|c| (c.key().to_vec(), c.value().to_vec(), c.together_count()))
        .collect();

    if combos.is_empty() {
        println!(
            "{key}={value}: no combinations stored (rebuild the sidecar with osmflat-extc --combinations)"
        );
        return Ok(());
    }

    println!("{key}={value}: top co-occurring tags\n");
    println!(
        "{:<28} {:<28} {:>12}",
        "other key", "other value", "together"
    );
    for (other_key, other_value, together_count) in combos.into_iter().take(top) {
        println!(
            "{:<28} {:<28} {:>12}",
            s(&other_key),
            s(&other_value),
            together_count
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
