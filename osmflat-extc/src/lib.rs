//! Compiler library: read an osmflat `Osm` archive (mmap) and emit an `Ext`
//! sidecar archive. The `osmflat-extc` binary is a thin CLI over [`build`].
//!
//! Build strategy (planet-scale, CSR; see `osmflat-ext-design.md` §6):
//! every postings vector is built **count-then-fill** so it can be backed by
//! mmap scratch instead of assuming it fits in RAM, mirroring osmflatc's
//! `--scratch-dir` philosophy.

use std::path::{Path, PathBuf};

pub mod build_backrefs;
pub mod build_taginfo;

/// What to build and where to spill.
#[derive(Debug, Clone, Default)]
pub struct BuildOptions {
    /// Build the Taginfo sub-archive.
    pub taginfo: bool,
    /// Build the Backrefs sub-archive.
    pub backrefs: bool,
    /// Also build taginfo key co-occurrence (phase 2).
    pub combinations: bool,
    /// Directory for mmap-backed postings scratch (planet scale).
    pub mmap_scratch: Option<PathBuf>,
}

/// Errors the compiler can surface.
#[derive(Debug)]
pub enum BuildError {
    /// Opening the parent or creating the output archive failed.
    Storage(flatdata::ResourceStorageError),
    /// I/O while writing the sidecar.
    Io(std::io::Error),
}

impl From<std::io::Error> for BuildError {
    fn from(e: std::io::Error) -> Self {
        BuildError::Io(e)
    }
}
impl From<flatdata::ResourceStorageError> for BuildError {
    fn from(e: flatdata::ResourceStorageError) -> Self {
        BuildError::Storage(e)
    }
}
impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Storage(e) => write!(f, "storage: {e:?}"),
            BuildError::Io(e) => write!(f, "io: {e}"),
        }
    }
}
impl std::error::Error for BuildError {}

/// Open the parent archive at `parent_dir`, build the requested sidecars, and
/// write the `Ext` archive to `out_dir`.
///
/// Steps:
/// 1. open parent (`osmflat::Osm::open`) and create the `ExtBuilder`;
/// 2. write the [`ExtHeader`](osmflat_ext::ExtHeader) fingerprint
///    ([`osmflat_ext::fingerprint::build_header`]) + this tool's version into
///    the sidecar's stringtable;
/// 3. if `opts.taginfo`, run [`build_taginfo::build`];
/// 4. if `opts.backrefs`, run [`build_backrefs::build`].
pub fn build(_parent_dir: &Path, _out_dir: &Path, _opts: &BuildOptions) -> Result<(), BuildError> {
    todo!(
        "open parent + create ExtBuilder; write fingerprint header; \
         dispatch to build_taginfo / build_backrefs"
    )
}
