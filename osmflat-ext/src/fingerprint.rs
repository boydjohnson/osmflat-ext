//! Staleness guard binding a sidecar to one exact parent build.
//!
//! A sidecar hard-codes parent **entity indices** and parent **stringtable
//! indices**; both change whenever the parent is recompiled or reordered. The
//! [`ExtHeader`](crate::ExtHeader) records a fingerprint of the parent at build
//! time, and [`verify`] compares it against a live parent before any query is
//! served. This is the ext analog of the `coord_scale` precondition in the
//! `osmflat-combine` design.

use crate::{Ext, ExtHeader};
use osmflat::Osm;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// Why a sidecar does not match a parent archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mismatch {
    /// The parent's `osm.flatdata` schema hash differs (a structural change).
    SchemaHash { expected: u64, actual: u64 },
    /// A parent vector length differs (a rebuild or reorder).
    Count {
        what: &'static str,
        expected: u64,
        actual: u64,
    },
    /// The parent's replication sequence number differs.
    ReplicationSequence { expected: i64, actual: i64 },
}

impl std::fmt::Display for Mismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ext archive was built against a different parent ({self:?}); rebuild with osmflat-extc"
        )
    }
}

impl std::error::Error for Mismatch {}

/// Hash identifying the parent schema this reader expects. Both the compiler
/// (when writing the header) and [`verify`] call this, so build and check
/// cannot drift. Uses the `osmflat` schema the binary is linked against; a
/// parent built with an incompatible schema would already fail to `open`.
pub fn parent_schema_hash(_parent: &Osm) -> u64 {
    let mut h = DefaultHasher::new();
    osmflat::schema::osm::OSM.hash(&mut h);
    h.finish()
}

/// Parent vector lengths (sentinel-trimmed, as the reader sees them) used as a
/// data fingerprint.
fn parent_counts(parent: &Osm) -> (u64, u64, u64, u64, u64) {
    (
        parent.nodes().len() as u64,
        parent.ways().len() as u64,
        parent.relations().len() as u64,
        parent.tags().len() as u64,
        parent.stringtable().as_bytes().len() as u64,
    )
}

/// Compute the fingerprint for a freshly-built sidecar of `parent`. The compiler
/// sets `builder_idx` into the sidecar's own stringtable and writes the result
/// as [`Ext`]'s header.
pub fn build_header(parent: &Osm, builder_idx: u64) -> ExtHeader {
    let (nodes, ways, relations, tags, stringtable_len) = parent_counts(parent);
    let mut h = ExtHeader::new();
    h.set_parent_schema_hash(parent_schema_hash(parent));
    h.set_parent_node_count(nodes);
    h.set_parent_way_count(ways);
    h.set_parent_relation_count(relations);
    h.set_parent_tags_count(tags);
    h.set_parent_stringtable_len(stringtable_len);
    h.set_parent_replication_sequence_number(parent.header().replication_sequence_number());
    h.set_builder_idx(builder_idx);
    h
}

/// Verify that `ext` was built against `parent`. Returns the first mismatch
/// found, or `Ok(())` when the sidecar is valid for this parent.
pub fn verify(parent: &Osm, ext: &Ext) -> Result<(), Mismatch> {
    let h = ext.header();

    let expected = parent_schema_hash(parent);
    if h.parent_schema_hash() != expected {
        return Err(Mismatch::SchemaHash {
            expected,
            actual: h.parent_schema_hash(),
        });
    }

    let (nodes, ways, relations, tags, stringtable_len) = parent_counts(parent);
    for (what, expected, actual) in [
        ("nodes", nodes, h.parent_node_count()),
        ("ways", ways, h.parent_way_count()),
        ("relations", relations, h.parent_relation_count()),
        ("tags", tags, h.parent_tags_count()),
        ("stringtable", stringtable_len, h.parent_stringtable_len()),
    ] {
        if expected != actual {
            return Err(Mismatch::Count {
                what,
                expected,
                actual,
            });
        }
    }

    let expected_seq = parent.header().replication_sequence_number();
    if h.parent_replication_sequence_number() != expected_seq {
        return Err(Mismatch::ReplicationSequence {
            expected: expected_seq,
            actual: h.parent_replication_sequence_number(),
        });
    }

    Ok(())
}
