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
#[cfg(feature = "test-support")]
pub mod test_support;

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
pub fn build(parent_dir: &Path, out_dir: &Path, opts: &BuildOptions) -> Result<(), BuildError> {
    let parent = osmflat::Osm::open(osmflat::FileResourceStorage::new(parent_dir.to_path_buf()))?;
    let storage = osmflat::FileResourceStorage::new(out_dir.to_path_buf());
    build_into(&parent, storage, opts)
}

/// Build the requested sidecars for an already-open parent archive into
/// `storage`.
pub fn build_into(
    parent: &osmflat::Osm,
    storage: flatdata::StorageHandle,
    opts: &BuildOptions,
) -> Result<(), BuildError> {
    let builder = osmflat_ext::ExtBuilder::new(storage)?;
    // This archive's own stringtable: index 0 is the empty string, the tool
    // version follows. `builder_idx` points at the version.
    let mut stringtable = vec![0u8];
    let builder_idx = stringtable.len() as u64;
    stringtable.extend_from_slice(concat!("osmflat-extc ", env!("CARGO_PKG_VERSION")).as_bytes());
    stringtable.push(0);
    builder.set_stringtable(&stringtable)?;

    let header = osmflat_ext::fingerprint::build_header(parent, builder_idx);
    builder.set_header(&header)?;

    if opts.taginfo {
        let taginfo = builder.taginfo()?;
        build_taginfo::build(parent, &taginfo, opts)?;
    }
    if opts.backrefs {
        let backrefs = builder.backrefs()?;
        build_backrefs::build(parent, &backrefs)?;
    }
    Ok(())
}
