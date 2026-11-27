//! Reading a result: every length and count is checked against the file
//! before anything is reserved, so no input can make the reader allocate
//! more than the file holds, and every structure is checksummed.

use std::sync::Arc;

use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use super::{
    BLOCK_ENTRY_BYTES, CRITICAL, Cursor, END_MAGIC, ENTRY_BYTES, FOOTER_BYTES, HEADER_BYTES,
    Identity, MAGIC, MAJOR, Meta, chunk_count, corrupt, kind, unzigzag,
};
use crate::budget::{Budget, Half, Reservation};
use crate::edit::{ApproxReason, Edit, Hunk};
use crate::error::{Error, Result};
use crate::index::{Checkpoint, LineIndex};
use crate::moves::MovedBlock;
use crate::sched::HUNK_OVERHEAD;
use crate::segment::{Segment, SegmentStatus};
use crate::source::Source;
use crate::types::{ByteOffset, Bytes, LineNo};
use crate::view::ApproxTally;

/// Most entries a section table may hold.
const MAX_ENTRIES: u64 = 64;
/// Largest meta section accepted: paths and a few numbers.
const MAX_META: u64 = 1 << 20;
/// Fewest bytes a segment record takes: tag, length, byte length.
const MIN_RECORD: u64 = 3;
/// Bytes a decoded segment takes in the map: the segment and where it
/// starts in both files, as the live diff holds it.
const SEGMENT_BYTES: u64 = (size_of::<Segment>() + size_of::<[ByteOffset; 2]>()) as u64;

/// One entry of the section table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    /// Section type, one of [`kind`] or a type this version does not know.
    pub kind: u16,
    /// Whether a reader must know the type.
    pub critical: bool,
    /// Where the section starts.
    pub offset: u64,
    /// Its length in bytes.
    pub length: u64,
    /// xxh3-64 of its bytes.
    pub hash: u64,
}

/// One segment block, as the block index lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockEntry {
    /// Number of the block's first segment.
    pub first: u64,
    /// First line of A and of B in the block.
    pub lines: [u64; 2],
    /// Where the block starts in the result.
    pub offset: u64,
    /// Its length, checksum included.
    pub len: u64,
}

/// A result file with its header, section table and meta read and
/// checked. The rest is read on demand.
#[derive(Debug)]
pub struct SavedFile<R> {
    src: R,
    version: (u16, u16),
    flags: u32,
    shift: u32,
    sections: Vec<Section>,
    meta: Meta,
    held: Reservation,
}

/// The segment map of a result, decoded and checked: every block's
/// checksum and structure, every script against its segment, and the
/// totals against the meta section.
#[derive(Debug)]
pub struct Map {
    /// Segments in file order; in the Skeleton half.
    pub segments: Vec<Segment>,
    /// Where each segment starts in A and in B; in the Skeleton half.
    pub starts: Vec<[ByteOffset; 2]>,
    /// The block index; in the Skeleton half.
    pub blocks: Vec<BlockEntry>,
    /// Longest block, in bytes.
    pub max_block: u64,
    /// Most heap one block's scripts take once decoded.
    pub max_decoded: u64,
    /// Approximate scripts, with their lines.
    pub approx: ApproxTally,
    _held: Reservation,
}

impl<R: Source> SavedFile<R> {
    /// Reads and checks the header, the footer, the section table and the
    /// meta section of `src`. The meta section's bytes are held in the
    /// Refine half for as long as the result is open.
    ///
    /// # Errors
    ///
    /// [`Error::Saved`] when `src` is not a result, is incomplete, needs a
    /// newer Rift, or fails a check; [`Error::Io`] from `src`;
    /// [`Error::Budget`] when the meta section does not fit.
    pub fn open(src: R, budget: &Budget) -> Result<Self> {
        let len = src.len().0;
        let mut header = [0u8; HEADER_BYTES as usize];
        if len < HEADER_BYTES + FOOTER_BYTES {
            return Err(Error::Saved("not a Rift result: too short".into()));
        }
        src.read_exact_at(&mut header, ByteOffset(0))?;
        if &header[0..8] != MAGIC {
            return Err(Error::Saved("not a Rift result".into()));
        }
        let mut h = Cursor::new(&header[8..]);
        let (major, minor) = (h.u16()?, h.u16()?);
        let (flags, shift) = (h.u32()?, h.u32()?);
        h.take(4)?;
        let tables = [[h.u64()?, h.u64()?], [h.u64()?, h.u64()?]];
        if !sum_ok(xxh3_64(&header[..56]), h.u64()?) {
            return Err(corrupt("header checksum"));
        }
        if major != MAJOR {
            return Err(Error::Saved(format!(
                "the result is in format {major}.{minor}; this Rift reads format {MAJOR}, \
                 so open it with the Rift that wrote it or recompute it"
            )));
        }
        if !(10..=30).contains(&shift) {
            return Err(corrupt("chunk size"));
        }
        let sections = read_table(&src, len)?;
        let meta_section = required(&sections, kind::META)?;
        if meta_section.length > MAX_META {
            return Err(corrupt("meta section too long"));
        }
        let mut file = Self {
            src,
            version: (major, minor),
            flags,
            shift,
            sections,
            meta: Meta::default(),
            held: budget.try_reserve(Half::Refine, Bytes(0))?,
        };
        let (bytes, held) = file.load(&meta_section, budget, Half::Refine)?;
        file.meta = decode_meta(&bytes)?;
        file.held = held;
        for (side, k) in [(0, kind::CHUNKS_A), (1, kind::CHUNKS_B)] {
            let s = required(&file.sections, k)?;
            let n = chunk_count(file.meta.files[side].size, shift);
            if tables[side] != [s.offset, n] || s.length != 8 * n {
                return Err(corrupt("chunk table does not match the file size"));
            }
        }
        Ok(file)
    }

    /// The format version, major and minor.
    #[must_use]
    pub fn version(&self) -> (u16, u16) {
        self.version
    }

    /// Header flags: [`FLAG_EDITS`](super::FLAG_EDITS) and the others.
    #[must_use]
    pub fn flags(&self) -> u32 {
        self.flags
    }

    /// Bytes per hash chunk, as a power of two.
    #[must_use]
    pub fn chunk_shift(&self) -> u32 {
        self.shift
    }

    /// Identity, parameters and summary.
    #[must_use]
    pub fn meta(&self) -> &Meta {
        &self.meta
    }

    /// Every section listed, unknown non-critical ones included.
    #[must_use]
    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    /// The section of type `kind`, if listed.
    #[must_use]
    pub fn section(&self, kind: u16) -> Option<Section> {
        self.sections.iter().copied().find(|s| s.kind == kind)
    }

    /// The bytes of `s`, checked against its hash, held in `half`.
    ///
    /// # Errors
    ///
    /// [`Error::Budget`] when they do not fit; [`Error::Io`] from the
    /// source; [`Error::Saved`] on a checksum mismatch.
    pub fn load(&self, s: &Section, budget: &Budget, half: Half) -> Result<(Vec<u8>, Reservation)> {
        let held = budget.try_reserve(half, Bytes(s.length))?;
        let n = usize::try_from(s.length).map_err(|_| corrupt("section too long"))?;
        let mut v = vec![0u8; n];
        self.src.read_exact_at(&mut v, ByteOffset(s.offset))?;
        if !sum_ok(xxh3_64(&v), s.hash) {
            return Err(corrupt(&format!("checksum of section type {}", s.kind)));
        }
        Ok((v, held))
    }

    /// The chunk hashes of A (`side` 0) or B, held in `half`.
    ///
    /// # Errors
    ///
    /// As for [`load`](Self::load).
    pub fn chunk_table(
        &self,
        side: usize,
        budget: &Budget,
        half: Half,
    ) -> Result<(Vec<u64>, Reservation)> {
        let k = [kind::CHUNKS_A, kind::CHUNKS_B][side];
        let s = required(&self.sections, k)?;
        let (raw, raw_held) = self.load(&s, budget, half)?;
        let held = budget.try_reserve(half, Bytes(s.length))?;
        let table = raw
            .as_chunks::<8>()
            .0
            .iter()
            .map(|c| u64::from_le_bytes(*c))
            .collect();
        drop((raw, raw_held));
        Ok((table, held))
    }

    /// Both line indexes, held in the Skeleton half.
    ///
    /// # Errors
    ///
    /// As for [`load`](Self::load), and [`Error::Saved`] when an index is
    /// inconsistent with the file it describes.
    pub fn line_indexes(&self, budget: &Budget) -> Result<([LineIndex; 2], Reservation)> {
        let s = required(&self.sections, kind::LINES)?;
        let (raw, _held) = self.load(&s, budget, Half::Skeleton)?;
        let mut c = Cursor::new(&raw);
        let mut held = budget.try_reserve(Half::Skeleton, Bytes(0))?;
        let a = self.line_index(&mut c, 0, &mut held)?;
        let b = self.line_index(&mut c, 1, &mut held)?;
        if c.left() != 0 {
            return Err(corrupt("line index has trailing bytes"));
        }
        Ok(([a, b], held))
    }

    fn line_index(
        &self,
        c: &mut Cursor<'_>,
        side: usize,
        held: &mut Reservation,
    ) -> Result<LineIndex> {
        let bad = || corrupt("line index");
        let interval = c.uvar()?;
        let lines = c.uvar()?;
        let n = c.uvar()?;
        let file_len = self.meta.files[side].size;
        if interval == 0
            || lines != self.meta.lines[side]
            || n > c.left() as u64 / 2
            || (lines > 0) != (n > 0)
        {
            return Err(bad());
        }
        held.grow(Bytes(n * size_of::<Checkpoint>() as u64))?;
        let mut cps = Vec::with_capacity(n as usize);
        let (mut line, mut offset) = (0u64, 0u64);
        for i in 0..n {
            let (dl, doff) = (c.uvar()?, c.uvar()?);
            let (l, o) = (
                line.checked_add(dl).ok_or_else(bad)?,
                offset.checked_add(doff).ok_or_else(bad)?,
            );
            let ok = if i == 0 {
                l == 0 && o == 0
            } else {
                l > line && o / interval > offset / interval
            };
            if !ok || l >= lines || o >= file_len {
                return Err(bad());
            }
            cps.push(Checkpoint {
                line: LineNo(l),
                offset: ByteOffset(o),
            });
            (line, offset) = (l, o);
        }
        Ok(LineIndex::new(cps, lines, Bytes(interval)))
    }

    /// The moved blocks, held in the Refine half, and whether the search
    /// was off, skipped for the budget, or stopped with its list full.
    ///
    /// # Errors
    ///
    /// As for [`load`](Self::load), and [`Error::Saved`] when a block lies
    /// outside the files.
    pub fn moves(&self, budget: &Budget) -> Result<(Vec<MovedBlock>, [bool; 3], Reservation)> {
        let bad = || corrupt("moves");
        let Some(s) = self.section(kind::MOVES) else {
            return Ok((
                Vec::new(),
                [true, false, false],
                budget.try_reserve(Half::Refine, Bytes(0))?,
            ));
        };
        let (raw, _held) = self.load(&s, budget, Half::Refine)?;
        let mut c = Cursor::new(&raw);
        let flags = c.u8()?;
        let n = c.uvar()?;
        if flags >> 3 != 0 || n > c.left() as u64 / 3 {
            return Err(bad());
        }
        let held = budget.try_reserve(Half::Refine, Bytes(n * size_of::<MovedBlock>() as u64))?;
        let mut blocks = Vec::with_capacity(n as usize);
        let mut last = 0u64;
        for _ in 0..n {
            let a = last.checked_add(c.uvar()?).ok_or_else(bad)?;
            let b = c.uvar()?;
            let len = c.uvar()?;
            let (ea, eb) = (
                a.checked_add(len).ok_or_else(bad)?,
                b.checked_add(len).ok_or_else(bad)?,
            );
            if ea > self.meta.lines[0] || eb > self.meta.lines[1] {
                return Err(bad());
            }
            blocks.push(MovedBlock {
                a: LineNo(a)..LineNo(ea),
                b: LineNo(b)..LineNo(eb),
            });
            last = ea;
        }
        if c.left() != 0 {
            return Err(bad());
        }
        let bit = |i: u8| flags & (1 << i) != 0;
        Ok((blocks, [bit(0), bit(1), bit(2)], held))
    }

    /// Decodes and checks the whole segment map: one pass over the
    /// segment blocks. The segments and the block index go to the
    /// Skeleton half, a buffer for the longest block to the Refine half
    /// while the pass runs.
    ///
    /// # Errors
    ///
    /// [`Error::Budget`] when they do not fit; [`Error::Io`] from the
    /// source; [`Error::Saved`] on any inconsistency.
    pub fn map(&self, budget: &Budget) -> Result<Map> {
        let meta = &self.meta;
        let seg_section = required(&self.sections, kind::SEGMENTS)?;
        let total = meta.segment_count();
        if total > seg_section.length / MIN_RECORD {
            return Err(corrupt("more segments than the section can hold"));
        }
        let ix = required(&self.sections, kind::BLOCK_INDEX)?;
        if !ix.length.is_multiple_of(BLOCK_ENTRY_BYTES) {
            return Err(corrupt("block index length"));
        }
        let (blocks, held) = self.block_index(&ix, &seg_section, total, budget)?;
        let max_block = blocks.iter().map(|b| b.len).max().unwrap_or(0);
        let buf_held = budget.try_reserve(Half::Refine, Bytes(max_block))?;
        let mut buf = vec![0u8; usize::try_from(max_block).map_err(|_| corrupt("block too long"))?];
        let mut segments = Vec::with_capacity(total as usize);
        let mut starts = Vec::with_capacity(total as usize);
        let mut section = Xxh3::new();
        let mut state = [0u64; 5];
        let mut tally = Tally::default();
        let mut max_decoded = 0;
        for (j, b) in blocks.iter().enumerate() {
            let count = blocks.get(j + 1).map_or(total, |n| n.first) - b.first;
            let bytes = &mut buf[..b.len as usize];
            self.src.read_exact_at(bytes, ByteOffset(b.offset))?;
            section.update(bytes);
            let mut decoded = 16 + 8 * count;
            state = walk_block(bytes, b, count, Some(state), |seg, start, script| {
                if let Some((approx, edits)) = script {
                    decoded += HUNK_OVERHEAD.0 + edits * size_of::<Edit>() as u64;
                    tally.refined += 1;
                    if let Some(r) = approx {
                        tally.approx[r as usize] += 1;
                        tally.approx_lines.segments += 1;
                        tally.approx_lines.a_lines += seg.len_a();
                        tally.approx_lines.b_lines += seg.len_b();
                    }
                }
                tally.status[seg.status as usize] += 1;
                segments.push(seg);
                starts.push(start.map(ByteOffset));
                Ok(())
            })?;
            max_decoded = max_decoded.max(decoded);
        }
        drop((buf, buf_held));
        if !sum_ok(section.digest(), seg_section.hash) {
            return Err(corrupt("checksum of the segment section"));
        }
        let [_, la, lb, ba, bb] = state;
        if [la, lb] != meta.lines
            || [ba, bb] != [meta.files[0].size, meta.files[1].size]
            || tally.status != meta.segments
            || tally.approx != meta.approx
            || tally.refined != meta.refined
        {
            return Err(corrupt("the map does not match its summary"));
        }
        Ok(Map {
            segments,
            starts,
            blocks,
            max_block,
            max_decoded,
            approx: tally.approx_lines,
            _held: held,
        })
    }

    /// The block index, checked against the segment section and the
    /// segment count, held in the Skeleton half with room for the map.
    fn block_index(
        &self,
        ix: &Section,
        seg_section: &Section,
        total: u64,
        budget: &Budget,
    ) -> Result<(Vec<BlockEntry>, Reservation)> {
        let n_blocks = ix.length / BLOCK_ENTRY_BYTES;
        let (raw, raw_held) = self.load(ix, budget, Half::Skeleton)?;
        let held = budget.try_reserve(
            Half::Skeleton,
            Bytes(n_blocks * size_of::<BlockEntry>() as u64 + total * SEGMENT_BYTES),
        )?;
        let mut blocks = Vec::with_capacity(n_blocks as usize);
        let seg_end = seg_section.offset + seg_section.length;
        let mut c = Cursor::new(&raw);
        for i in 0..n_blocks {
            let first = c.u64()?;
            let lines = [c.u64()?, c.u64()?];
            let offset = c.u64()?;
            let next = if i + 1 < n_blocks {
                // The next entry's offset, 24 bytes into it.
                Cursor::new(&raw[c.pos + 24..]).u64()?
            } else {
                seg_end
            };
            let expect_offset = blocks
                .last()
                .map_or(seg_section.offset, |b: &BlockEntry| b.offset + b.len);
            let ok_first = blocks
                .last()
                .map_or(first == 0, |b: &BlockEntry| first > b.first);
            if offset != expect_offset
                || next <= offset
                || next > seg_end
                || !ok_first
                || first >= total
            {
                return Err(corrupt("block index"));
            }
            blocks.push(BlockEntry {
                first,
                lines,
                offset,
                len: next - offset,
            });
        }
        if (n_blocks == 0) != (total == 0) || (n_blocks == 0 && seg_section.length != 0) {
            return Err(corrupt("block index"));
        }
        drop((raw, raw_held));
        Ok((blocks, held))
    }

    /// The scripts of block `j` of `map`, one per segment of the block:
    /// `None` for an equal segment or one saved without a script. Reads
    /// the block into `buf`, which must hold [`Map::max_block`] bytes.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] from the source; [`Error::Saved`] when the block no
    /// longer matches the map read at open.
    pub fn block_hunks(
        &self,
        map: &Map,
        j: usize,
        buf: &mut [u8],
    ) -> Result<Vec<Option<Arc<Hunk>>>> {
        let b = &map.blocks[j];
        let total = map.segments.len() as u64;
        let count = map.blocks.get(j + 1).map_or(total, |n| n.first) - b.first;
        let bytes = &mut buf[..usize::try_from(b.len).map_err(|_| corrupt("block too long"))?];
        self.src.read_exact_at(bytes, ByteOffset(b.offset))?;
        let mut out = Vec::with_capacity(count as usize);
        let mut k = b.first as usize;
        walk_collect(bytes, b, count, |seg, hunk| {
            if map.segments.get(k) != Some(&seg) {
                return Err(corrupt("a block changed since the result was opened"));
            }
            k += 1;
            out.push(hunk.map(Arc::new));
            Ok(())
        })?;
        Ok(out)
    }
}

/// The footer and the section table of a result of `len` bytes, checked:
/// every section in bounds, each known type once, every required type
/// present, no unknown critical type.
fn read_table<R: Source>(src: &R, len: u64) -> Result<Vec<Section>> {
    let mut footer = [0u8; FOOTER_BYTES as usize];
    src.read_exact_at(&mut footer, ByteOffset(len - FOOTER_BYTES))?;
    if &footer[24..32] != END_MAGIC {
        return Err(Error::Saved(
            "incomplete result: it has no footer, so the save did not finish".into(),
        ));
    }
    let mut f = Cursor::new(&footer);
    let table = f.u64()?;
    let entries = u64::from(f.u32()?);
    f.take(4)?;
    let table_hash = f.u64()?;
    if entries > MAX_ENTRIES || table.checked_add(entries * ENTRY_BYTES) != Some(len - FOOTER_BYTES)
    {
        return Err(corrupt("section table position"));
    }
    let mut raw = [0u8; (MAX_ENTRIES * ENTRY_BYTES) as usize];
    let raw = &mut raw[..(entries * ENTRY_BYTES) as usize];
    src.read_exact_at(raw, ByteOffset(table))?;
    if !sum_ok(xxh3_64(raw), table_hash) {
        return Err(corrupt("section table checksum"));
    }
    let mut sections = Vec::with_capacity(entries as usize);
    let mut seen = 0u32;
    let mut c = Cursor::new(raw);
    for _ in 0..entries {
        let kind = c.u16()?;
        let critical = c.u16()? & CRITICAL != 0;
        c.take(4)?;
        let s = Section {
            kind,
            critical,
            offset: c.u64()?,
            length: c.u64()?,
            hash: c.u64()?,
        };
        if s.offset < HEADER_BYTES || s.offset.checked_add(s.length).is_none_or(|e| e > table) {
            return Err(corrupt("section out of bounds"));
        }
        if (kind::META..=kind::CHUNKS_B).contains(&kind) {
            if seen & (1 << kind) != 0 {
                return Err(corrupt("section listed twice"));
            }
            seen |= 1 << kind;
        } else if critical {
            return Err(Error::Saved(format!(
                "the result needs section type {kind}, which a newer Rift writes; \
                 open it with that version or recompute it"
            )));
        }
        sections.push(s);
    }
    if (kind::META..=kind::CHUNKS_B).any(|k| seen & (1 << k) == 0) {
        return Err(corrupt("a required section is missing"));
    }
    Ok(sections)
}

/// The section of type `kind`, which [`read_table`] found present.
fn required(sections: &[Section], kind: u16) -> Result<Section> {
    sections
        .iter()
        .copied()
        .find(|s| s.kind == kind)
        .ok_or_else(|| corrupt("a required section is missing"))
}

/// Whether a checksum matches. A fuzzing build takes every one as good,
/// so its inputs reach the checks behind them.
fn sum_ok(got: u64, want: u64) -> bool {
    cfg!(fuzzing) || got == want
}

#[derive(Default)]
struct Tally {
    status: [u64; 3],
    approx: [u64; 3],
    refined: u64,
    approx_lines: ApproxTally,
}

/// A script as the map pass sees it: its reason, and how many edits.
type ScriptInfo = Option<(Option<ApproxReason>, u64)>;

/// Walks the `count` records of `block`, listed as `entry`, checking its
/// checksum and its structure, and calls `f` for each segment with where
/// it starts in A and in B. With
/// `state`, the running segment number, lines and bytes, checks the block
/// header against it. Returns the state after the block.
fn walk_block(
    block: &[u8],
    entry: &BlockEntry,
    count: u64,
    state: Option<[u64; 5]>,
    mut f: impl FnMut(Segment, [u64; 2], ScriptInfo) -> Result<()>,
) -> Result<[u64; 5]> {
    walk(block, entry, count, state, false, |seg, start, info, _| {
        f(seg, start, info)
    })
}

/// [`walk_block`] that builds each script.
fn walk_collect(
    block: &[u8],
    entry: &BlockEntry,
    count: u64,
    mut f: impl FnMut(Segment, Option<Hunk>) -> Result<()>,
) -> Result<[u64; 5]> {
    walk(block, entry, count, None, true, |seg, _, _, hunk| {
        f(seg, hunk)
    })
}

fn walk(
    block: &[u8],
    entry: &BlockEntry,
    count: u64,
    state: Option<[u64; 5]>,
    collect: bool,
    mut f: impl FnMut(Segment, [u64; 2], ScriptInfo, Option<Hunk>) -> Result<()>,
) -> Result<[u64; 5]> {
    let bad = || corrupt("segment block");
    let Some(body_len) = block.len().checked_sub(8) else {
        return Err(bad());
    };
    let (body, sum) = block.split_at(body_len);
    if !sum_ok(xxh3_64(body), Cursor::new(sum).u64()?) {
        return Err(corrupt("segment block checksum"));
    }
    let mut c = Cursor::new(body);
    if c.u8()? != 1 {
        return Err(bad());
    }
    if c.u8()? != 0 {
        return Err(Error::Saved(
            "the result uses a block encoding this Rift does not read".into(),
        ));
    }
    let head = [c.uvar()?, c.uvar()?, c.uvar()?, c.uvar()?, c.uvar()?];
    if head[0] != entry.first || head[1..3] != entry.lines || state.is_some_and(|s| s != head) {
        return Err(bad());
    }
    let [mut n, mut la, mut lb, mut ba, mut bb] = head;
    let add = |x: u64, y: u64| x.checked_add(y).ok_or_else(bad);
    for _ in 0..count {
        let tag = c.u8()?;
        let status = match tag & 3 {
            0 => SegmentStatus::Equal,
            1 => SegmentStatus::Differs,
            2 => SegmentStatus::Approx,
            _ => return Err(bad()),
        };
        let refined = tag & 4 != 0;
        let reason = match tag >> 3 {
            0 => None,
            r @ 1..=3 if refined => Some(ApproxReason::ALL[usize::from(r - 1)]),
            _ => return Err(bad()),
        };
        let equal = status == SegmentStatus::Equal;
        if equal && refined {
            return Err(bad());
        }
        let len_a = c.uvar()?;
        let len_b = if equal { len_a } else { c.uvar()? };
        let bytes_a = c.uvar()?;
        let bytes_b = if equal { bytes_a } else { c.uvar()? };
        if len_a == 0 && len_b == 0 {
            return Err(bad());
        }
        let seg = Segment {
            a: LineNo(la)..LineNo(add(la, len_a)?),
            b: LineNo(lb)..LineNo(add(lb, len_b)?),
            status,
        };
        let (mut info, mut hunk) = (None, None);
        if refined {
            let (edits, out) = decode_edits(&mut c, &seg, collect)?;
            info = Some((reason, edits));
            hunk = out.map(|edits| Hunk {
                edits,
                approx: reason,
            });
        }
        let start = [ba, bb];
        (la, lb) = (seg.a.end.0, seg.b.end.0);
        (ba, bb) = (add(ba, bytes_a)?, add(bb, bytes_b)?);
        n += 1;
        f(seg, start, info, hunk)?;
    }
    if c.left() != 0 {
        return Err(bad());
    }
    Ok([n, la, lb, ba, bb])
}

/// The `edits` of a script for `seg`, read from `c` and checked to cover
/// it exactly; built only when `collect` is set.
fn decode_edits(
    c: &mut Cursor<'_>,
    seg: &Segment,
    collect: bool,
) -> Result<(u64, Option<Vec<Edit>>)> {
    let bad = || corrupt("segment block");
    let add = |x: u64, y: u64| x.checked_add(y).ok_or_else(bad);
    let edits = c.uvar()?;
    if edits > c.left() as u64 {
        return Err(bad());
    }
    let mut out = collect.then(|| Vec::with_capacity(edits as usize));
    let (mut a, mut b) = (seg.a.start.0, seg.b.start.0);
    for _ in 0..edits {
        let v = c.uvar()?;
        let len = v >> 2;
        let e = match v & 3 {
            0 => {
                let e = Edit::Equal {
                    a: LineNo(a),
                    b: LineNo(b),
                    len,
                };
                a = add(a, len)?;
                b = add(b, len)?;
                e
            }
            1 => {
                let e = Edit::Delete { a: LineNo(a), len };
                a = add(a, len)?;
                e
            }
            2 => {
                let e = Edit::Insert { b: LineNo(b), len };
                b = add(b, len)?;
                e
            }
            _ => return Err(bad()),
        };
        if let Some(out) = &mut out {
            out.push(e);
        }
    }
    if a != seg.a.end.0 || b != seg.b.end.0 {
        return Err(bad());
    }
    Ok((edits, out))
}

fn decode_meta(bytes: &[u8]) -> Result<Meta> {
    let bad = || corrupt("meta section");
    let mut m = Meta::default();
    let mut c = Cursor::new(bytes);
    let mut seen = 0u64;
    while c.left() > 0 {
        let tag = c.uvar()?;
        let len = c.uvar()?;
        let mut r = Cursor::new(c.take(len)?);
        let mut nums = |out: &mut [u64]| -> Result<()> {
            for v in out {
                *v = r.uvar()?;
            }
            Ok(())
        };
        let bit = match tag {
            1 | 2 => {
                let mut id = Identity::default();
                let mut v = [0u64; 5];
                nums(&mut v)?;
                id.size = v[0];
                id.mtime_secs = unzigzag(v[1]);
                id.mtime_nanos = u32::try_from(v[2]).map_err(|_| bad())?;
                (id.dev, id.ino) = (v[3], v[4]);
                let n = r.uvar()?;
                id.path = r.take(n)?.to_vec();
                let n = r.uvar()?;
                id.canonical = r.take(n)?.to_vec();
                m.files[tag as usize - 1] = id;
                tag
            }
            16 => {
                let mut v = [0];
                nums(&mut v)?;
                m.mem = v[0];
                3
            }
            17 => {
                let mut v = [0];
                nums(&mut v)?;
                m.moves = v[0] != 0;
                4
            }
            18 => {
                m.version = String::from_utf8_lossy(r.take(len)?).into_owned();
                5
            }
            32 => {
                nums(&mut m.lines)?;
                6
            }
            33 => {
                nums(&mut m.segments)?;
                7
            }
            34 => {
                nums(&mut m.approx)?;
                8
            }
            35 => {
                let mut v = [0];
                nums(&mut v)?;
                m.refined = v[0];
                9
            }
            // Records a later minor adds; this version skips them.
            _ => continue,
        };
        seen |= 1 << bit;
    }
    if seen != 0b11_1111_1110 {
        return Err(corrupt("meta section lacks a record"));
    }
    if m.segments
        .iter()
        .try_fold(0u64, |s, &v| s.checked_add(v))
        .is_none()
        || m.refined > m.differing()
    {
        return Err(bad());
    }
    Ok(m)
}
