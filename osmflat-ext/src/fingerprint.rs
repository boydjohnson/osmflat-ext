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

/// Hash the parent's embedded schema bytes (`osm.flatdata`).
///
/// Must use the same hash the compiler used when writing the header; keep the
/// algorithm in one place so build and verify cannot drift (cf. the spatial
/// `node_curve`/`way_curve` sharing).
pub fn parent_schema_hash(_parent: &Osm) -> u64 {
    todo!("hash the parent archive's embedded osm.flatdata schema bytes")
}

/// Compute the fingerprint fields for a freshly-built sidecar of `parent`.
///
/// The compiler calls this, sets `builder_idx` into the sidecar's own
/// stringtable, and writes the result as [`Ext`]'s header.
pub fn build_header(_parent: &Osm, _builder_idx: u64) -> ExtHeader {
    todo!(
        "fill ExtHeader from parent: schema hash, node/way/relation/tags counts, \
         stringtable len, replication sequence"
    )
}

/// Verify that `ext` was built against `parent`. Returns the first mismatch
/// found, or `Ok(())` when the sidecar is valid for this parent.
pub fn verify(_parent: &Osm, _ext: &Ext) -> Result<(), Mismatch> {
    todo!(
        "compare ExtHeader fields against the live parent: schema hash, vector \
         lengths, replication sequence"
    )
}
