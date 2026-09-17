//! Fixed-size `u64` scratch arrays for the count-then-fill CSR builds.
//!
//! [`Scratch::alloc`] returns a zero-filled array backed either by RAM or, when
//! a scratch directory is configured (`--mmap-scratch`), by an mmap'd unnamed
//! temp file in that directory — so postings and offset arrays sized by the
//! parent (design §6.4) never have to fit in RAM. Temp files are created with
//! [`tempfile::tempfile_in`], i.e. already unlinked: the OS reclaims the space
//! when the mapping drops, even on a crash.

use crate::BuildError;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
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

/// Append-only sink of `(u64, u64)` records: a growable RAM buffer, or a
/// sequential-write unnamed temp file in a scratch directory. Writes are
/// purely append -- no positional seeking -- which is the cheapest I/O pattern
/// when the records don't fit in RAM.
pub(crate) enum PairSink {
    Ram(Vec<(u64, u64)>),
    File { writer: BufWriter<File>, len: usize },
}

impl PairSink {
    pub(crate) fn new(dir: Option<&Path>) -> Result<Self, BuildError> {
        match dir {
            Some(dir) => {
                std::fs::create_dir_all(dir)?;
                Ok(PairSink::File {
                    writer: BufWriter::new(tempfile::tempfile_in(dir)?),
                    len: 0,
                })
            }
            None => Ok(PairSink::Ram(Vec::new())),
        }
    }

    pub(crate) fn push(&mut self, a: u64, b: u64) -> Result<(), BuildError> {
        match self {
            PairSink::Ram(v) => v.push((a, b)),
            PairSink::File { writer, len } => {
                writer.write_all(&a.to_le_bytes())?;
                writer.write_all(&b.to_le_bytes())?;
                *len += 1;
            }
        }
        Ok(())
    }

    /// Append a batch of records, in order.
    pub(crate) fn extend(&mut self, records: &[(u64, u64)]) -> Result<(), BuildError> {
        match self {
            PairSink::Ram(v) => v.extend_from_slice(records),
            PairSink::File { writer, len } => {
                for &(a, b) in records {
                    writer.write_all(&a.to_le_bytes())?;
                    writer.write_all(&b.to_le_bytes())?;
                }
                *len += records.len();
            }
        }
        Ok(())
    }

    /// Consume the sink and return all records, in insertion order.
    pub(crate) fn into_records(self) -> Result<Vec<(u64, u64)>, BuildError> {
        match self {
            PairSink::Ram(v) => Ok(v),
            PairSink::File { writer, len } => {
                let mut file = writer.into_inner().map_err(|e| e.into_error())?;
                file.seek(SeekFrom::Start(0))?;
                // Decode through a bounded buffer rather than reading the
                // whole file first, so a bucket never needs twice its size
                // in RAM.
                let mut records = Vec::with_capacity(len);
                let mut buf = vec![0u8; 16 * 64 * 1024];
                let mut remaining = len;
                while remaining > 0 {
                    let n = remaining.min(buf.len() / 16);
                    let chunk = &mut buf[..n * 16];
                    file.read_exact(chunk)?;
                    records.extend(chunk.as_chunks::<16>().0.iter().map(|c| {
                        let (a, b) = c.split_at(8);
                        (
                            u64::from_le_bytes(a.try_into().unwrap()),
                            u64::from_le_bytes(b.try_into().unwrap()),
                        )
                    }));
                    remaining -= n;
                }
                Ok(records)
            }
        }
    }
}
