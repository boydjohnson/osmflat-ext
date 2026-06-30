#![allow(clippy::all)] // generated code is not clippy friendly
#![allow(unknown_lints)]
#![allow(mismatched_lifetime_syntaxes)] // generated flatdata bindings

//! Sidecar indexes and query support for [osmflat] archives.
//!
//! `osmflat-ext` adds the capabilities the base format can't do efficiently —
//! an **inverted tag index** + **taginfo histograms**, **reverse references**,
//! and **non-bbox spatial** queries — *without modifying the parent `Osm`
//! archive*. Everything lives in a separate sibling [`Ext`] archive (built by
//! `osmflat-extc`) whose resources reference the parent by index:
//!
//! * string `*_idx` fields are indices into the **parent** `Osm.stringtable`;
//! * [`Ref`] values are indices into the **parent** `nodes`/`ways`/`relations`.
//!
//! Because the parent stores entities in space-filling-curve order, the
//! postings in this archive (ascending parent indices) are *already spatially
//! ordered*, so a `key=value` postings list merge-joins directly with a bbox
//! query — see [`query`].
//!
//! A sidecar is only valid for the exact parent build it was compiled from; the
//! [`fingerprint`] module guards against using a stale one.
//!
//! Design doc: `osmflat-ext-design.md`.
//!
//! [osmflat]: https://docs.rs/osmflat

// generated osm_ext module (regenerate with `flatdata/regen.sh`)
include!("ext_generated.rs");

pub mod backrefs;
pub mod fingerprint;
pub mod query;
pub mod spatial;
pub mod taginfo;

pub use crate::osm_ext::*;

// re-export what callers need to open archives without depending on flatdata
pub use flatdata::FileResourceStorage;

/// A parent archive paired with its verified extension sidecar.
///
/// Construct with [`ExtArchive::open`], which opens both and checks the
/// [`fingerprint`] before returning, so every query method can assume the
/// sidecar matches the parent.
pub struct ExtArchive {
    parent: osmflat::Osm,
    ext: Ext,
}

impl ExtArchive {
    /// Open a parent `Osm` archive and its sibling `Ext` archive, verifying the
    /// fingerprint. Returns [`fingerprint::Mismatch`] if the sidecar was built
    /// against a different parent.
    pub fn open(parent: osmflat::Osm, ext: Ext) -> Result<Self, fingerprint::Mismatch> {
        fingerprint::verify(&parent, &ext)?;
        Ok(Self { parent, ext })
    }

    /// The underlying parent archive.
    #[inline]
    pub fn parent(&self) -> &osmflat::Osm {
        &self.parent
    }

    /// The underlying extension archive.
    #[inline]
    pub fn ext(&self) -> &Ext {
        &self.ext
    }

    /// Taginfo / inverted-tag-index queries. `None` if the sidecar was built
    /// without `--taginfo`.
    #[inline]
    pub fn taginfo(&self) -> Option<taginfo::TaginfoQuery<'_>> {
        taginfo::TaginfoQuery::new(&self.parent, self.ext.taginfo()?).into()
    }

    /// Reverse-reference queries. `None` if built without `--backrefs`.
    #[inline]
    pub fn backrefs(&self) -> Option<backrefs::BackrefsQuery<'_>> {
        backrefs::BackrefsQuery::new(&self.parent, self.ext.backrefs()?).into()
    }
}
