//! Writing a result in one forward pass.

use std::cell::RefCell;

use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use super::{
    BLOCK_ENTRY_BYTES, BLOCK_TARGET, CHUNK_SHIFT, CRITICAL, ChunkTable, END_MAGIC, ENTRY_BYTES,
    FLAG_COMPLETE, FLAG_EDITS, FLAG_MOVES, HEADER_BYTES, Identity, MAGIC, MAJOR, MAX_SECTIONS,
    MINOR, Meta, Sink, chunk_count, kind, uvar, zigzag,
};
use crate::budget::{Budget, Half, Reservation};
use crate::diff::{Diff, Written};
use crate::edit::{ApproxReason, Edit, Hunk};
use crate::error::{Error, Result};
use crate::index::LineIndex;
use crate::moves::Moves;
use crate::segment::{Segment, SegmentStatus};
use crate::skeleton::Skeleton;
use crate::source::Source;
use crate::types::{ByteOffset, Bytes};
use crate::unified::Options;

/// One section table entry, as written.
#[derive(Debug, Clone, Copy)]
struct Entry {
    kind: u16,
    flags: u16,
    offset: u64,
    length: u64,
    hash: u64,
}

/// Writes a result to a [`Sink`] in one forward pass.
///
/// [`new`](Self::new) sets room aside for the two chunk tables, which the
/// scan fills in place through [`chunk_tables`](Self::chunk_tables). Then
/// every segment goes through [`push`](Self::push) in file order, and
/// [`finish`](Self::finish) appends the other sections, the section table,
/// the footer and finally the header. A sink that holds no footer is not a
/// result; a caller that writes to a temporary file and renames it after
/// `finish` never leaves a partial result under the final name.
///
/// Memory: a block buffer of [`BLOCK_TARGET`] bytes and room for
/// [`MAX_SECTIONS`] entries from the Refine half, taken in `new`, and the
/// block index from the Skeleton half, grown as blocks close.
pub struct Writer<'s> {
    sink: &'s dyn Sink,
    sizes: [u64; 2],
    chunks: [u64; 2],
    /// Where `buf` goes when flushed.
    pos: u64,
    buf: Vec<u8>,
    sections: Vec<Entry>,
    /// Hash of the section being written.
    section: Xxh3,
    /// Hash of the block being written, when one is open.
    block: Option<Xxh3>,
    block_len: u64,
    blocks: Vec<[u64; 4]>,
    segments_start: u64,
    /// Whether the segment section is closed: no segment may follow.
    segments_done: bool,
    /// The next segment's number, first lines and first bytes.
    next: [u64; 5],
    meta: Meta,
    budget: Budget,
    _refine: Reservation,
    index: Reservation,
}

impl std::fmt::Debug for Writer<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Writer")
            .field("pos", &self.pos)
            .field("blocks", &self.blocks.len())
            .finish_non_exhaustive()
    }
}

impl<'s> Writer<'s> {
    /// A writer for the result of two files of `sizes` bytes.
    ///
    /// # Errors
    ///
    /// [`Error::Budget`] when the Refine half cannot hold the block buffer
    /// and the section table.
    pub fn new(sink: &'s dyn Sink, sizes: [Bytes; 2], budget: &Budget) -> Result<Self> {
        let refine = budget.try_reserve(
            Half::Refine,
            Bytes(BLOCK_TARGET + MAX_SECTIONS as u64 * size_of::<Entry>() as u64),
        )?;
        let index = budget.try_reserve(Half::Skeleton, Bytes(0))?;
        let sizes = [sizes[0].0, sizes[1].0];
        let chunks = sizes.map(|s| chunk_count(s, CHUNK_SHIFT));
        let start = HEADER_BYTES + 8 * (chunks[0] + chunks[1]);
        Ok(Self {
            sink,
            sizes,
            chunks,
            pos: start,
            buf: Vec::with_capacity(BLOCK_TARGET as usize),
            sections: Vec::with_capacity(MAX_SECTIONS),
            section: Xxh3::new(),
            block: None,
            block_len: 0,
            blocks: Vec::new(),
            segments_start: start,
            segments_done: false,
            next: [0; 5],
            meta: Meta::default(),
            budget: budget.clone(),
            _refine: refine,
            index,
        })
    }

    /// Where the scan writes the chunk hashes of A and of B.
    #[must_use]
    pub fn chunk_tables(&self) -> [ChunkTable<'s>; 2] {
        [
            ChunkTable::new(self.sink, HEADER_BYTES, CHUNK_SHIFT),
            ChunkTable::new(self.sink, HEADER_BYTES + 8 * self.chunks[0], CHUNK_SHIFT),
        ]
    }

    /// Appends `seg`, the next segment in file order, whose ranges hold
    /// `bytes` bytes of A and of B, with its edit script when it has one.
    /// An equal segment keeps no script: it is implied.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidScript`] when `seg` does not follow the previous
    /// segment or its script does not cover it; [`Error::Budget`] when the
    /// block index cannot grow; [`Error::Write`] from the sink.
    pub fn push(&mut self, seg: &Segment, bytes: [u64; 2], hunk: Option<&Hunk>) -> Result<()> {
        let [n, la, lb, ba, bb] = self.next;
        if self.segments_done {
            return Err(Error::InvalidScript(format!(
                "segment {n} pushed after another section"
            )));
        }
        if seg.a.start.0 != la || seg.b.start.0 != lb {
            return Err(Error::InvalidScript(format!(
                "segment {n} starts at {}/{}, expected {la}/{lb}",
                seg.a.start, seg.b.start
            )));
        }
        let equal = seg.status == SegmentStatus::Equal;
        if equal && (seg.len_a() != seg.len_b() || bytes[0] != bytes[1]) {
            return Err(Error::InvalidScript(format!(
                "equal segment {n} has sides of different length"
            )));
        }
        if self.block.is_none() {
            self.open_block()?;
        }
        let status = match seg.status {
            SegmentStatus::Equal => 0u8,
            SegmentStatus::Differs => 1,
            SegmentStatus::Approx => 2,
        };
        let script = hunk.filter(|_| !equal);
        let mut tag = status;
        if let Some(h) = script {
            check_script(seg, h)?;
            tag |= 1 << 2;
            if let Some(r) = h.approx {
                tag |= (r as u8 + 1) << 3;
                self.meta.approx[r as usize] += 1;
            }
            self.meta.refined += 1;
        }
        self.meta.segments[status as usize] += 1;
        self.emit(&[tag], true)?;
        self.var(seg.len_a())?;
        if !equal {
            self.var(seg.len_b())?;
        }
        self.var(bytes[0])?;
        if !equal {
            self.var(bytes[1])?;
        }
        if let Some(h) = script {
            self.var(h.edits.len() as u64)?;
            for e in &h.edits {
                let (k, len) = match *e {
                    Edit::Equal { len, .. } => (0, len),
                    Edit::Delete { len, .. } => (1, len),
                    Edit::Insert { len, .. } => (2, len),
                };
                if len >> 62 != 0 {
                    return Err(Error::InvalidScript(format!("edit of {len} lines")));
                }
                self.var((len << 2) | k)?;
            }
        }
        let overflow = || Error::InvalidScript("segment ends past 2^64".into());
        self.next = [
            n + 1,
            seg.a.end.0,
            seg.b.end.0,
            ba.checked_add(bytes[0]).ok_or_else(overflow)?,
            bb.checked_add(bytes[1]).ok_or_else(overflow)?,
        ];
        if self.block_len >= BLOCK_TARGET {
            self.close_block()?;
        }
        Ok(())
    }

    /// Appends a section of `kind` holding `data`, after the last segment:
    /// no segment may be pushed once a section is added. A later format
    /// minor adds its sections through this; a reader that does not know
    /// `kind` skips it unless `critical`.
    ///
    /// # Errors
    ///
    /// [`Error::Saved`] when the section table is full; [`Error::Write`]
    /// from the sink.
    pub fn section(&mut self, kind: u16, critical: bool, data: &[u8]) -> Result<()> {
        self.end_segments()?;
        let start = self.begin();
        self.emit(data, false)?;
        self.end(kind, critical, start)
    }

    /// Writes the rest: the meta section with `files` and the run's
    /// `mem` and move setting, the block index, both line indexes, the
    /// moved blocks, the section table, the footer and the header. The
    /// sink holds a complete result once this returns; it is not synced.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidScript`] when the segments pushed do not tile both
    /// files; [`Error::Saved`] or [`Error::Write`] from the sink.
    pub fn finish(
        mut self,
        files: [Identity; 2],
        mem: Bytes,
        indexes: [&LineIndex; 2],
        moves: &Moves,
    ) -> Result<()> {
        let [_, la, lb, ba, bb] = self.next;
        if [ba, bb] != self.sizes || [la, lb] != indexes.map(LineIndex::line_count) {
            return Err(Error::InvalidScript(format!(
                "segments cover {la}/{lb} lines and {ba}/{bb} bytes of {}/{} lines and {}/{} bytes",
                indexes[0].line_count(),
                indexes[1].line_count(),
                self.sizes[0],
                self.sizes[1]
            )));
        }
        self.end_segments()?;

        let start = self.begin();
        for b in std::mem::take(&mut self.blocks) {
            for v in b {
                self.emit(&v.to_le_bytes(), false)?;
            }
        }
        self.end(kind::BLOCK_INDEX, true, start)?;

        let start = self.begin();
        for ix in indexes {
            self.line_index(ix)?;
        }
        self.end(kind::LINES, true, start)?;

        let start = self.begin();
        let flags = u8::from(moves.disabled())
            | u8::from(moves.skipped()) << 1
            | u8::from(moves.full()) << 2;
        self.emit(&[flags], false)?;
        self.var(moves.blocks().len() as u64)?;
        let mut last = 0;
        for m in moves.blocks() {
            self.var(m.a.start.0 - last)?;
            self.var(m.b.start.0)?;
            self.var(m.a.end - m.a.start)?;
            last = m.a.end.0;
        }
        self.end(kind::MOVES, false, start)?;

        self.meta.files = files;
        self.meta.mem = mem.0;
        self.meta.moves = !moves.disabled();
        self.meta.lines = [la, lb];
        let start = self.begin();
        self.meta_records()?;
        self.end(kind::META, true, start)?;

        // The scan wrote the tables in place; hash them back.
        self.flush()?;
        for (side, k) in [(0, kind::CHUNKS_A), (1, kind::CHUNKS_B)] {
            let offset = if side == 0 {
                HEADER_BYTES
            } else {
                HEADER_BYTES + 8 * self.chunks[0]
            };
            let len = 8 * self.chunks[side];
            let hash = self.hash_back(offset, len)?;
            self.push_entry(k, true, offset, len, hash)?;
        }

        let table = self.end_pos();
        let mut h = Xxh3::new();
        for e in std::mem::take(&mut self.sections) {
            let mut b = [0u8; ENTRY_BYTES as usize];
            b[0..2].copy_from_slice(&e.kind.to_le_bytes());
            b[2..4].copy_from_slice(&e.flags.to_le_bytes());
            b[8..16].copy_from_slice(&e.offset.to_le_bytes());
            b[16..24].copy_from_slice(&e.length.to_le_bytes());
            b[24..32].copy_from_slice(&e.hash.to_le_bytes());
            h.update(&b);
            self.emit(&b, false)?;
        }
        let entries = (self.end_pos() - table) / ENTRY_BYTES;
        let mut footer = [0u8; 32];
        footer[0..8].copy_from_slice(&table.to_le_bytes());
        // At most MAX_SECTIONS entries.
        footer[8..12].copy_from_slice(&(entries as u32).to_le_bytes());
        footer[16..24].copy_from_slice(&h.digest().to_le_bytes());
        footer[24..32].copy_from_slice(END_MAGIC);
        self.emit(&footer, false)?;
        self.flush()?;

        let header = self.header();
        self.sink
            .write_at(&header, ByteOffset(0))
            .map_err(Error::Write)
    }

    fn header(&self) -> [u8; HEADER_BYTES as usize] {
        let mut flags = FLAG_MOVES;
        if self.meta.refined > 0 {
            flags |= FLAG_EDITS;
        }
        if self.meta.refined == self.meta.differing() {
            flags |= FLAG_COMPLETE;
        }
        let mut header = [0u8; HEADER_BYTES as usize];
        header[0..8].copy_from_slice(MAGIC);
        header[8..10].copy_from_slice(&MAJOR.to_le_bytes());
        header[10..12].copy_from_slice(&MINOR.to_le_bytes());
        header[12..16].copy_from_slice(&flags.to_le_bytes());
        header[16..20].copy_from_slice(&CHUNK_SHIFT.to_le_bytes());
        header[24..32].copy_from_slice(&HEADER_BYTES.to_le_bytes());
        header[32..40].copy_from_slice(&self.chunks[0].to_le_bytes());
        header[40..48].copy_from_slice(&(HEADER_BYTES + 8 * self.chunks[0]).to_le_bytes());
        header[48..56].copy_from_slice(&self.chunks[1].to_le_bytes());
        let hash = xxh3_64(&header[..56]);
        header[56..64].copy_from_slice(&hash.to_le_bytes());
        header
    }

    fn line_index(&mut self, ix: &LineIndex) -> Result<()> {
        self.var(ix.interval().0)?;
        self.var(ix.line_count())?;
        self.var(ix.checkpoints().len() as u64)?;
        let (mut line, mut offset) = (0, 0);
        for c in ix.checkpoints() {
            self.var(c.line.0 - line)?;
            self.var(c.offset.0 - offset)?;
            (line, offset) = (c.line.0, c.offset.0);
        }
        Ok(())
    }

    fn meta_records(&mut self) -> Result<()> {
        let mut rec = Vec::new();
        let files = std::mem::take(&mut self.meta.files);
        for (tag, f) in [(1, &files[0]), (2, &files[1])] {
            rec.clear();
            for v in [
                f.size,
                zigzag(f.mtime_secs),
                u64::from(f.mtime_nanos),
                f.dev,
                f.ino,
            ] {
                push_var(&mut rec, v);
            }
            for s in [&f.path, &f.canonical] {
                push_var(&mut rec, s.len() as u64);
                rec.extend_from_slice(s);
            }
            self.record(tag, &rec)?;
        }
        self.meta.files = files;
        let m = self.meta.clone();
        let nums: [(u64, &[u64]); 6] = [
            (16, &[m.mem]),
            (17, &[u64::from(m.moves)]),
            (32, &m.lines),
            (33, &m.segments),
            (34, &m.approx),
            (35, &[m.refined]),
        ];
        for (tag, vals) in nums {
            rec.clear();
            for &v in vals {
                push_var(&mut rec, v);
            }
            self.record(tag, &rec)?;
        }
        self.record(18, env!("CARGO_PKG_VERSION").as_bytes())
    }

    fn record(&mut self, tag: u64, data: &[u8]) -> Result<()> {
        self.var(tag)?;
        self.var(data.len() as u64)?;
        self.emit(data, false)
    }

    fn open_block(&mut self) -> Result<()> {
        if self.blocks.len() == self.blocks.capacity() {
            let extra = self.blocks.len().max(16);
            self.index.grow(Bytes(extra as u64 * BLOCK_ENTRY_BYTES))?;
            self.blocks.try_reserve_exact(extra).map_err(|_| {
                Error::Budget(crate::BudgetExceeded {
                    half: Half::Skeleton,
                    requested: Bytes(extra as u64 * BLOCK_ENTRY_BYTES),
                    available: self.budget.available(Half::Skeleton),
                })
            })?;
        }
        let [n, la, lb, ba, bb] = self.next;
        self.blocks.push([n, la, lb, self.end_pos()]);
        self.block = Some(Xxh3::new());
        self.block_len = 0;
        self.emit(&[1, 0], true)?;
        for v in [n, la, lb, ba, bb] {
            self.var(v)?;
        }
        Ok(())
    }

    fn close_block(&mut self) -> Result<()> {
        let hash = self.block.take().expect("a block is open").digest();
        self.emit(&hash.to_le_bytes(), false)
    }

    /// Closes the last block and the segment section, once.
    fn end_segments(&mut self) -> Result<()> {
        if self.segments_done {
            return Ok(());
        }
        if self.block.is_some() {
            self.close_block()?;
        }
        self.segments_done = true;
        let start = self.segments_start;
        let hash = self.section.digest();
        self.push_entry(kind::SEGMENTS, true, start, self.end_pos() - start, hash)
    }

    fn begin(&mut self) -> u64 {
        self.section.reset();
        self.end_pos()
    }

    fn end(&mut self, kind: u16, critical: bool, start: u64) -> Result<()> {
        let hash = self.section.digest();
        self.push_entry(kind, critical, start, self.end_pos() - start, hash)
    }

    fn push_entry(
        &mut self,
        kind: u16,
        critical: bool,
        offset: u64,
        length: u64,
        hash: u64,
    ) -> Result<()> {
        if self.sections.len() == MAX_SECTIONS {
            return Err(Error::Saved(format!("more than {MAX_SECTIONS} sections")));
        }
        self.sections.push(Entry {
            kind,
            flags: if critical { CRITICAL } else { 0 },
            offset,
            length,
            hash,
        });
        Ok(())
    }

    fn end_pos(&self) -> u64 {
        self.pos + self.buf.len() as u64
    }

    fn var(&mut self, v: u64) -> Result<()> {
        let mut b = [0u8; 10];
        let n = uvar(v, &mut b);
        self.emit(&b[..n], true)
    }

    /// Appends `bytes` to the section, and to the open block too when
    /// `block` is set and one is open.
    fn emit(&mut self, mut bytes: &[u8], block: bool) -> Result<()> {
        self.section.update(bytes);
        if block && let Some(h) = &mut self.block {
            h.update(bytes);
            self.block_len += bytes.len() as u64;
        }
        while !bytes.is_empty() {
            let room = self.buf.capacity() - self.buf.len();
            let n = room.min(bytes.len());
            self.buf.extend_from_slice(&bytes[..n]);
            bytes = &bytes[n..];
            if self.buf.len() == self.buf.capacity() {
                self.flush()?;
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.sink
            .write_at(&self.buf, ByteOffset(self.pos))
            .map_err(Error::Write)?;
        self.pos += self.buf.len() as u64;
        self.buf.clear();
        Ok(())
    }

    /// xxh3-64 of `len` bytes at `offset`, read back through the block
    /// buffer, which must be empty.
    fn hash_back(&mut self, offset: u64, len: u64) -> Result<u64> {
        let mut h = Xxh3::new();
        let cap = self.buf.capacity();
        self.buf.resize(cap, 0);
        let mut at = 0;
        while at < len {
            let n = usize::try_from(len - at).unwrap_or(usize::MAX).min(cap);
            let res = self
                .sink
                .read_exact_at(&mut self.buf[..n], ByteOffset(offset + at));
            if let Err(e) = res {
                self.buf.clear();
                return Err(Error::Io(e));
            }
            h.update(&self.buf[..n]);
            at += n as u64;
        }
        self.buf.clear();
        Ok(h.digest())
    }
}

fn push_var(out: &mut Vec<u8>, v: u64) {
    let mut b = [0u8; 10];
    let n = uvar(v, &mut b);
    out.extend_from_slice(&b[..n]);
}

/// Whether `h` covers `seg` exactly, as the `Hunk` invariant says.
fn check_script(seg: &Segment, h: &Hunk) -> Result<()> {
    let (mut a, mut b) = (seg.a.start.0, seg.b.start.0);
    for e in &h.edits {
        let ok = match *e {
            Edit::Equal { a: x, b: y, len } => {
                let ok = x.0 == a && y.0 == b;
                a += len;
                b += len;
                ok
            }
            Edit::Delete { a: x, len } => {
                let ok = x.0 == a;
                a += len;
                ok
            }
            Edit::Insert { b: y, len } => {
                let ok = y.0 == b;
                b += len;
                ok
            }
        };
        if !ok {
            return Err(Error::InvalidScript(format!(
                "edits of segment at {}/{} are not contiguous",
                seg.a.start, seg.b.start
            )));
        }
    }
    if a != seg.a.end.0 || b != seg.b.end.0 {
        return Err(Error::InvalidScript(format!(
            "edits of segment at {}/{} end at {a}/{b}, not {}/{}",
            seg.a.start, seg.b.start, seg.a.end, seg.b.end
        )));
    }
    Ok(())
}

/// Writes the unified diff of `diff` to `out`, as
/// [`Diff::write_unified`] does, and pushes every segment with its
/// script to `writer` on the way, then finishes the result with `files`
/// and the run's `mem`.
///
/// `diff` must have been built with `writer`'s
/// [`chunk_tables`](Writer::chunk_tables).
///
/// # Errors
///
/// As for [`Diff::write_unified`] and [`Writer::finish`].
pub fn write_diff<A, B>(
    diff: &Diff<A, B>,
    mut writer: Writer<'_>,
    files: [Identity; 2],
    mem: Bytes,
    opts: &Options<'_>,
    out: impl std::io::Write,
) -> Result<Written>
where
    A: Source + Send + ?Sized + 'static,
    B: Source + Send + ?Sized + 'static,
{
    let segs = crate::DiffView::segments(diff);
    let starts = diff.starts();
    let sizes = writer.sizes;
    let bytes = |i: usize| -> [u64; 2] {
        let end = starts.get(i + 1).map_or(sizes, |s| [s[0].0, s[1].0]);
        [end[0] - starts[i][0].0, end[1] - starts[i][1].0]
    };
    let state = RefCell::new((&mut writer, 0usize));
    let written = diff.write_unified_with(opts, out, |i, h| {
        let (w, next) = &mut *state.borrow_mut();
        while *next < i {
            w.push(&segs[*next], bytes(*next), None)?;
            *next += 1;
        }
        if *next == i {
            w.push(&segs[i], bytes(i), Some(h))?;
            *next += 1;
        }
        Ok(())
    })?;
    let next = state.into_inner().1;
    for (i, s) in segs.iter().enumerate().skip(next) {
        writer.push(s, bytes(i), None)?;
    }
    writer.finish(files, mem, diff.indexes(), diff.moves())?;
    Ok(written)
}

/// Writes the map of a `--summary` run: every segment without a script.
///
/// # Errors
///
/// As for [`Writer::push`] and [`Writer::finish`].
pub fn write_map(
    map: &Skeleton,
    mut writer: Writer<'_>,
    files: [Identity; 2],
    mem: Bytes,
    indexes: [&LineIndex; 2],
    moves: &Moves,
) -> Result<()> {
    let (segs, starts) = (map.segments(), map.starts());
    for (i, s) in segs.iter().enumerate() {
        let end = starts.get(i + 1).map_or(writer.sizes, |s| [s[0].0, s[1].0]);
        writer.push(s, [end[0] - starts[i][0].0, end[1] - starts[i][1].0], None)?;
    }
    writer.finish(files, mem, indexes, moves)
}

// The reason is stored as its index plus one in two bits.
const _: () = assert!(ApproxReason::ALL.len() == 3);
