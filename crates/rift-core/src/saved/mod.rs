//! Saved results: the `.riftdiff` format.
//!
//! A saved result keeps the structure of a diff, not its text: the segment
//! map with byte lengths, the edit script of every refined segment, the
//! moved blocks, both files' sparse line indexes, and an xxh3-64 hash of
//! every 1 MiB chunk of both files. The text stays in the two files, and
//! every read of it is checked against the chunk hashes, so a result never
//! shows text from a file that changed since the save.
//!
//! The layout is specified in `docs/riftdiff.md`. In short: a 64-byte
//! header, the two chunk tables, the segment blocks, the other sections,
//! a section table and a 32-byte footer. The header, the section table,
//! every section and every block carry an xxh3-64. A reader refuses an
//! unknown major version and an unknown section marked critical, and
//! skips an unknown section that is not.
//!
//! The core stays free of the file system: [`Writer`] writes through a
//! [`Sink`], [`SavedFile`] reads through a [`Source`].
//!
//! Memory. Writing: the block buffer and the section table from the
//! Refine half, taken before the diff so the scheduler gets what is left;
//! the block index, 32 bytes per block of about 64 KiB, from the Skeleton
//! half, which the samples have left by the time blocks are written. The
//! chunk tables are written in place by the scan and take no heap.
//! Opening: see [`SavedDiff`].

// The format names the two files a and b, and its fields after them.
#![allow(
    clippy::many_single_char_names,
    clippy::similar_names,
    reason = "a and b are the two files, as in the format"
)]
// Lengths that reach a cast are checked against the file first, and Rift
// builds for 64-bit targets only.
#![allow(
    clippy::cast_possible_truncation,
    reason = "lengths checked; 64-bit targets"
)]

mod read;
mod view;
mod write;

#[cfg(test)]
mod tests;

use std::io;

use crate::edit::ApproxReason;
use crate::error::{Error, Result};
use crate::source::Source;
use crate::types::ByteOffset;

pub use read::{BlockEntry, Map, SavedFile, Section};
pub use view::{SavedDiff, Verified};
pub use write::{Writer, write_diff, write_map};

/// Bytes at the start of every result.
pub const MAGIC: &[u8; 8] = b"RIFTDIFF";
/// Bytes at the end of every complete result.
pub const END_MAGIC: &[u8; 8] = b"RIFTEND\0";
/// The format major this code writes and reads.
pub const MAJOR: u16 = 1;
/// The format minor this code writes.
pub const MINOR: u16 = 0;
/// Fixed header size.
pub const HEADER_BYTES: u64 = 64;
/// Fixed footer size.
pub const FOOTER_BYTES: u64 = 32;
/// One section table entry.
pub const ENTRY_BYTES: u64 = 32;
/// One block index entry.
pub const BLOCK_ENTRY_BYTES: u64 = 32;
/// Hash chunks are `1 << CHUNK_SHIFT` bytes: 1 MiB, fixed, so the table
/// does not follow `--mem` the way the line index does.
pub const CHUNK_SHIFT: u32 = 20;
/// Segment blocks close once they hold this many bytes; a segment's
/// record is never split, so a block can be larger.
pub const BLOCK_TARGET: u64 = 64 << 10;
/// Most sections a result may list.
pub const MAX_SECTIONS: usize = 32;

/// Header flag: the result holds edit scripts.
pub const FLAG_EDITS: u32 = 1;
/// Header flag: every differing segment has its edit script.
pub const FLAG_COMPLETE: u32 = 1 << 1;
/// Header flag: the result holds a moves section.
pub const FLAG_MOVES: u32 = 1 << 2;

/// Section table entry flag: a reader that does not know the type must
/// refuse the file.
pub const CRITICAL: u16 = 1;

/// Section types. Numbers are never reused.
pub mod kind {
    /// File identity, run parameters and the summary, as tagged records.
    pub const META: u16 = 1;
    /// One entry per segment block: first segment, first lines, offset.
    pub const BLOCK_INDEX: u16 = 2;
    /// The run of segment blocks.
    pub const SEGMENTS: u16 = 3;
    /// The sparse line indexes of A and B.
    pub const LINES: u16 = 4;
    /// Moved blocks and whether they were searched.
    pub const MOVES: u16 = 5;
    /// xxh3-64 of every fixed-size chunk of A.
    pub const CHUNKS_A: u16 = 6;
    /// xxh3-64 of every fixed-size chunk of B.
    pub const CHUNKS_B: u16 = 7;
    /// Reserved: content-defined chunks of A with xxh3-128 hashes, for an
    /// incremental re-diff. Not critical; this version skips it.
    pub const CHUNKS_CDC_A: u16 = 8;
    /// Reserved: as [`CHUNKS_CDC_A`] for B.
    pub const CHUNKS_CDC_B: u16 = 9;
}

/// Where a result is written: positional writes, and reads of what was
/// written, which the writer needs to hash the chunk tables the scan
/// filled in place.
///
/// Implementations must allow writes at different offsets from several
/// threads at once: the scan's workers write their chunk hashes in
/// parallel.
pub trait Sink: Source {
    /// Writes all of `buf` at `offset`, extending the sink as needed.
    ///
    /// # Errors
    ///
    /// The underlying I/O error.
    fn write_at(&self, buf: &[u8], offset: ByteOffset) -> io::Result<()>;
}

/// Where the scan writes the chunk hashes of one file: hash `k`, of bytes
/// `k << shift .. (k + 1) << shift`, goes to `offset + 8 * k`, little
/// endian.
#[derive(Clone, Copy)]
pub struct ChunkTable<'s> {
    pub(crate) sink: &'s dyn Sink,
    pub(crate) offset: u64,
    pub(crate) shift: u32,
}

impl std::fmt::Debug for ChunkTable<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChunkTable")
            .field("offset", &self.offset)
            .field("shift", &self.shift)
            .finish_non_exhaustive()
    }
}

impl<'s> ChunkTable<'s> {
    /// A table at `offset` of `sink` for chunks of `1 << shift` bytes.
    ///
    /// # Panics
    ///
    /// Panics if `shift` is not in `1..=30`.
    #[must_use]
    pub fn new(sink: &'s dyn Sink, offset: u64, shift: u32) -> Self {
        assert!((1..=30).contains(&shift), "chunk shift {shift}");
        Self {
            sink,
            offset,
            shift,
        }
    }

    pub(crate) fn put(&self, chunk: u64, hash: u64) -> Result<()> {
        self.sink
            .write_at(&hash.to_le_bytes(), ByteOffset(self.offset + 8 * chunk))
            .map_err(Error::Write)
    }
}

/// Chunks of `1 << shift` bytes in a file of `len` bytes.
#[must_use]
pub fn chunk_count(len: u64, shift: u32) -> u64 {
    len.div_ceil(1 << shift)
}

/// One file as the result knows it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
    /// The path as given on the command line, raw bytes.
    pub path: Vec<u8>,
    /// The canonical path, raw bytes; empty if unknown.
    pub canonical: Vec<u8>,
    /// Length in bytes.
    pub size: u64,
    /// Modification time, seconds since the Unix epoch.
    pub mtime_secs: i64,
    /// Nanoseconds of the modification time.
    pub mtime_nanos: u32,
    /// Device number.
    pub dev: u64,
    /// Inode number.
    pub ino: u64,
}

/// What a result holds besides the map: identity, parameters, summary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Meta {
    /// A and B. Not covered by the determinism promise: it differs
    /// between copies and machines.
    pub files: [Identity; 2],
    /// The `--mem` of the run.
    pub mem: u64,
    /// Whether moved blocks were looked for.
    pub moves: bool,
    /// The Rift version that wrote the result.
    pub version: String,
    /// Lines in A and B.
    pub lines: [u64; 2],
    /// Segments by status: equal, differs, approx.
    pub segments: [u64; 3],
    /// Refined segments whose hunk is approximate, by reason, indexed like
    /// [`ApproxReason::ALL`].
    pub approx: [u64; ApproxReason::ALL.len()],
    /// Differing segments with an edit script.
    pub refined: u64,
}

impl Meta {
    /// Segments in the map.
    #[must_use]
    pub fn segment_count(&self) -> u64 {
        self.segments.iter().sum()
    }

    /// Segments that are not equal.
    #[must_use]
    pub fn differing(&self) -> u64 {
        self.segments[1] + self.segments[2]
    }
}

/// A result that cannot be read, with the reason.
#[cold]
pub(crate) fn corrupt(what: &str) -> Error {
    Error::Saved(format!("corrupt result: {what}"))
}

/// Appends `v` as LEB128 to `out`, returning the bytes used.
pub(crate) fn uvar(mut v: u64, out: &mut [u8; 10]) -> usize {
    let mut n = 0;
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out[n] = byte;
            return n + 1;
        }
        out[n] = byte | 0x80;
        n += 1;
    }
}

pub(crate) fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)).cast_unsigned()
}

pub(crate) fn unzigzag(v: u64) -> i64 {
    (v >> 1).cast_signed() ^ -((v & 1).cast_signed())
}

/// Reads fields from a byte slice; every read fails cleanly at the end.
pub(crate) struct Cursor<'b> {
    pub(crate) bytes: &'b [u8],
    pub(crate) pos: usize,
}

impl<'b> Cursor<'b> {
    pub(crate) fn new(bytes: &'b [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    pub(crate) fn left(&self) -> usize {
        self.bytes.len() - self.pos
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        let b = *self
            .bytes
            .get(self.pos)
            .ok_or_else(|| corrupt("record ends early"))?;
        self.pos += 1;
        Ok(b)
    }

    pub(crate) fn uvar(&mut self) -> Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.u8()?;
            let part = u64::from(b & 0x7f);
            if shift == 63 && part > 1 {
                return Err(corrupt("integer overflows 64 bits"));
            }
            v |= part << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(corrupt("integer overflows 64 bits"))
    }

    pub(crate) fn take(&mut self, n: u64) -> Result<&'b [u8]> {
        let n = usize::try_from(n)
            .ok()
            .filter(|&n| n <= self.left())
            .ok_or_else(|| corrupt("record ends early"))?;
        let s = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    pub(crate) fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub(crate) fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        let (first, _) = self.bytes[self.pos..]
            .split_first_chunk::<N>()
            .ok_or_else(|| corrupt("record ends early"))?;
        self.pos += N;
        Ok(*first)
    }
}
