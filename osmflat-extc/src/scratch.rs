//! Fixed-size `u64` scratch arrays for the count-then-fill CSR builds.
//!
//! [`Scratch::alloc`] returns a zero-filled array backed either by RAM or, when
//! a scratch directory is configured (`--mmap-scratch`), by an mmap'd unnamed
//! temp file in that directory — so postings and offset arrays sized by the
//! parent (design §6.4) never have to fit in RAM. Temp files are created with
//! [`tempfile::tempfile_in`], i.e. already unlinked: the OS reclaims the space
//! when the mapping drops, even on a crash.

use crate::BuildError;
use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};

/// Allocator for scratch arrays: RAM-backed, or mmap-backed in `dir`.
pub(crate) struct Scratch {
    dir: Option<PathBuf>,
}

impl Scratch {
    pub(crate) fn new(dir: Option<&Path>) -> Self {
        Scratch {
            dir: dir.map(Path::to_path_buf),
        }
    }

    /// Allocate a zero-filled array of `len` u64s.
    pub(crate) fn alloc(&self, len: usize) -> Result<ScratchU64, BuildError> {
        // A zero-length file can't be mapped; RAM costs nothing here.
        let backing = match &self.dir {
            Some(dir) if len > 0 => {
                std::fs::create_dir_all(dir)?;
                let file = tempfile::tempfile_in(dir)?;
                file.set_len((len * std::mem::size_of::<u64>()) as u64)?;
                // Safety: the file is unnamed (unlinked at creation), so no
                // other process can resize or write it under the mapping.
                let mmap = unsafe { memmap2::MmapMut::map_mut(&file)? };
                Backing::Mmap(mmap)
            }
            _ => Backing::Ram(vec![0; len]),
        };
        Ok(ScratchU64 { backing, len })
    }

    /// Allocate a copy of `src`.
    pub(crate) fn alloc_copy(&self, src: &[u64]) -> Result<ScratchU64, BuildError> {
        let mut out = self.alloc(src.len())?;
        out.copy_from_slice(src);
        Ok(out)
    }
}

/// A fixed-size `u64` array; derefs to `[u64]`.
pub(crate) struct ScratchU64 {
    backing: Backing,
    len: usize,
}

enum Backing {
    Ram(Vec<u64>),
    Mmap(memmap2::MmapMut),
}

impl Deref for ScratchU64 {
    type Target = [u64];

    fn deref(&self) -> &[u64] {
        match &self.backing {
            Backing::Ram(v) => v,
            // Safety: the mapping is page-aligned (satisfies u64 alignment),
            // was sized to exactly `len * 8` bytes, and lives as long as
            // `self`. `set_len` zero-fills, so every u64 is initialized.
            Backing::Mmap(m) => unsafe {
                std::slice::from_raw_parts(m.as_ptr() as *const u64, self.len)
            },
        }
    }
}

impl DerefMut for ScratchU64 {
    fn deref_mut(&mut self) -> &mut [u64] {
        match &mut self.backing {
            Backing::Ram(v) => v,
            // Safety: see `Deref`; the mapping is writable (`MmapMut`).
            Backing::Mmap(m) => unsafe {
                std::slice::from_raw_parts_mut(m.as_mut_ptr() as *mut u64, self.len)
            },
        }
    }
}
