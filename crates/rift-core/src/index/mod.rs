//! Sparse line index: a checkpoint offset at least every C lines.
//!
//! The scanner records one checkpoint per `interval` lines inside each of
//! its chunks, so consecutive checkpoints are at most `interval` lines apart
//! and a chunk boundary only brings them closer. [`IndexedLines`] turns the
//! index plus a [`Source`] into [`LineAccess`]: reaching any line costs one
//! binary search and a forward scan of fewer than `interval` lines.

use std::borrow::Borrow;
use std::io;
use std::ops::Range;
use std::sync::{Mutex, PoisonError};

use memchr::memchr_iter;

use crate::budget::{Budget, Half, Reservation};
use crate::error::{Error, Result};
use crate::lines::{LineAccess, LineBuf};
use crate::source::Source;
use crate::types::{ByteOffset, Bytes, LineNo};

/// A known line start: 16 bytes, so a 1 GiB pair indexed every 2 048 lines
/// costs well under a mebibyte.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct Checkpoint {
    /// Line number of the line that starts here.
    pub line: LineNo,
    /// Byte offset of that line's first byte.
    pub offset: ByteOffset,
}

const _: () = assert!(size_of::<Checkpoint>() == 16);

/// Line starts known for one file, sorted by line, the first one at line 0
/// whenever the file has any lines.
#[derive(Debug, Clone)]
pub struct LineIndex {
    checkpoints: Vec<Checkpoint>,
    lines: u64,
    interval: u64,
}

impl LineIndex {
    /// Builds an index from sorted checkpoints at most `interval` lines
    /// apart. Lives in the skeleton half together with the samples; the
    /// scanner holds the reservation.
    #[must_use]
    pub(crate) fn new(checkpoints: Vec<Checkpoint>, lines: u64, interval: u64) -> Self {
        debug_assert!(checkpoints.is_sorted_by_key(|c| c.line));
        debug_assert!(lines == 0 || checkpoints.first().map(|c| c.line) == Some(LineNo(0)));
        debug_assert!(
            checkpoints
                .windows(2)
                .all(|w| w[1].line - w[0].line <= interval)
        );
        Self {
            checkpoints,
            lines,
            interval,
        }
    }

    /// Lines in the file.
    #[must_use]
    pub fn line_count(&self) -> u64 {
        self.lines
    }

    /// Upper bound on the lines between two consecutive checkpoints.
    #[must_use]
    pub fn interval(&self) -> u64 {
        self.interval
    }

    /// All checkpoints, in line order.
    #[must_use]
    pub fn checkpoints(&self) -> &[Checkpoint] {
        &self.checkpoints
    }

    /// The last checkpoint at or before `line`, or `None` past the end.
    #[must_use]
    pub fn checkpoint_before(&self, line: LineNo) -> Option<Checkpoint> {
        if line.0 >= self.lines {
            return None;
        }
        let i = self.checkpoints.partition_point(|c| c.line <= line);
        Some(self.checkpoints[i - 1])
    }
}

/// [`LineAccess`] over a [`Source`] and its [`LineIndex`].
///
/// Reads go through one scratch buffer of fixed size, reserved from the
/// half the caller names, so concurrent readers serialize on it; give each
/// thread its own instance when that matters. The source and the index are
/// held by value: a borrow, or an `Arc` for a reader that must be `'static`.
///
/// The reader remembers the line start its last read reached, so reading
/// forward in small steps, as the unified writer and the refiner do, scans
/// each line once instead of once per step from the checkpoint before it.
#[derive(Debug)]
pub struct IndexedLines<S, I> {
    source: S,
    index: I,
    scratch: Mutex<Scratch>,
    _reservation: Reservation,
}

/// The read buffer, the line start the last read reached, and the one
/// the caller last pointed to with `seek`.
#[derive(Debug)]
struct Scratch {
    buf: Vec<u8>,
    memo: Option<Checkpoint>,
    hint: Option<Checkpoint>,
    /// What the last `read_lines` left at the front of `buf`: the line
    /// that starts there and how many bytes follow, so the next read can
    /// start from them instead of reading them again.
    held: Option<(Checkpoint, usize)>,
}

impl Scratch {
    /// Where a read of `line` can start inside the held bytes, if they
    /// help: when a known line start after `line` lies there, `line` is
    /// among them, and the scan starts from the closest held line start
    /// before it; otherwise only when the start a read would pick anyway
    /// lies there and the read would not step back instead.
    fn held_start(&self, cp: Checkpoint, line: LineNo) -> Option<Checkpoint> {
        let (first, len) = self.held?;
        let end = first.offset.0 + len as u64;
        let inside = |c: &Checkpoint| c.offset.0 >= first.offset.0 && c.offset.0 <= end;
        let known = [self.memo, self.hint, Some(cp)];
        let known = known.iter().flatten();
        let before = *known
            .clone()
            .filter(|c| c.line <= line)
            .max_by_key(|c| c.line)
            .unwrap_or(&cp);
        let after = known.filter(|c| c.line > line).min_by_key(|c| c.line);
        if first.line <= line && after.is_some_and(inside) {
            return Some(if inside(&before) { before } else { first });
        }
        let steps_back = after.is_some_and(|m| m.line - line <= BACK_LINES.min(line - before.line));
        (!steps_back && inside(&before)).then_some(before)
    }
}

/// Lines a reader steps back from the remembered line start instead of
/// scanning forward from a checkpoint: the context before a change, which
/// the unified writer reads right before the change it has sought to.
const BACK_LINES: u64 = 16;

/// Where a forward scan for `line` starts: the closest known line start
/// at or before it, among the checkpoint before it and the remembered
/// ones, or, when a remembered start is a few lines after `line` and
/// closer than that, the start of `line` itself, found by reading backward.
fn scan_start<S: Source>(
    source: &S,
    index: &LineIndex,
    known: [Option<Checkpoint>; 2],
    line: LineNo,
    scratch: &mut [u8],
) -> Result<Option<Checkpoint>> {
    let Some(cp) = index.checkpoint_before(line) else {
        return Ok(None);
    };
    let known = known.into_iter().flatten();
    let before = known
        .clone()
        .filter(|m| m.line <= line)
        .max_by_key(|m| m.line);
    let start = before.filter(|m| m.line > cp.line).unwrap_or(cp);
    let after = known.filter(|m| m.line > line).min_by_key(|m| m.line);
    if let Some(m) = after
        && m.line - line <= BACK_LINES.min(line - start.line)
        && let Some(found) = back_from(source, m, line, scratch)?
    {
        return Ok(Some(found));
    }
    Ok(Some(start))
}

/// The start of `line`, found by reading at most two buffers backward from
/// the line start `m` after it; `None` when it is further back than that.
fn back_from<S: Source>(
    source: &S,
    m: Checkpoint,
    line: LineNo,
    scratch: &mut [u8],
) -> Result<Option<Checkpoint>> {
    // The newline ending line `line - 1` is the (d + 1)-th before `m`.
    let mut need = m.line - line + 1;
    let mut end = m.offset.0;
    for _ in 0..2 {
        if end == 0 {
            break;
        }
        let from = end.saturating_sub(scratch.len() as u64);
        let buf = &mut scratch[..Bytes(end - from).as_usize()];
        source.read_exact_at(buf, ByteOffset(from))?;
        for nl in memchr::memrchr_iter(b'\n', buf) {
            need -= 1;
            if need == 0 {
                return Ok(Some(Checkpoint {
                    line,
                    offset: ByteOffset(from + nl as u64 + 1),
                }));
            }
        }
        end = from;
    }
    // Only line 0 starts without a newline before it.
    Ok((end == 0 && need == 1).then_some(Checkpoint {
        line,
        offset: ByteOffset(0),
    }))
}

impl<S: Source, I: Borrow<LineIndex>> IndexedLines<S, I> {
    /// Reserves a `scratch`-byte read buffer from `half`.
    ///
    /// A requested line longer than `scratch` is reported as
    /// [`Error::LineTooLong`]; lines that are only skipped over may be any
    /// length.
    ///
    /// # Errors
    ///
    /// [`Error::Budget`] if `half` cannot hold `scratch` bytes.
    pub fn new(source: S, index: I, budget: &Budget, half: Half, scratch: Bytes) -> Result<Self> {
        let reservation = budget.try_reserve(half, scratch)?;
        Ok(Self {
            source,
            index,
            scratch: Mutex::new(Scratch {
                buf: vec![0; scratch.as_usize()],
                memo: None,
                hint: None,
                held: None,
            }),
            _reservation: reservation,
        })
    }
}

impl<S: Source, I: Borrow<LineIndex> + Sync> LineAccess for IndexedLines<S, I> {
    fn line_count(&self) -> u64 {
        self.index.borrow().line_count()
    }

    fn read_lines(&self, range: Range<LineNo>, buf: &mut LineBuf) -> Result<()> {
        let count = self.index.borrow().line_count();
        if range.end.0 > count {
            return Err(Error::LineOutOfRange {
                line: range.end,
                count,
            });
        }
        buf.clear();
        if range.is_empty() {
            return Ok(());
        }
        let mut guard = self.scratch.lock().unwrap_or_else(PoisonError::into_inner);
        let (start, mut filled) = self.read_start(&mut guard, range.start)?;
        let Scratch {
            buf: scratch,
            memo,
            held,
            ..
        } = &mut *guard;
        let cap = scratch.len();
        let mut line = start.line.0;
        let mut pos = start.offset.0;
        // The line starting at `scratch[0]`, unless a long line was cut.
        let mut first = Some(start.line);
        // Held bytes are scanned before anything is read.
        let mut unread = filled;
        loop {
            let from = if unread > 0 {
                unread = 0;
                0
            } else {
                let n = read_retrying(
                    &self.source,
                    &mut scratch[filled..],
                    ByteOffset(pos + filled as u64),
                )?;
                if n == 0 {
                    if filled > 0 && line >= range.start.0 {
                        buf.push(&scratch[..filled])?;
                        line += 1;
                    }
                    return ended(line, range.end);
                }
                filled += n;
                filled - n
            };
            let end = filled;
            let mut line_start = 0usize;
            for nl in memchr_iter(b'\n', &scratch[from..end]).map(|i| i + from) {
                if line >= range.start.0 {
                    buf.push(&scratch[line_start..=nl])?;
                }
                line += 1;
                line_start = nl + 1;
                if line == range.end.0 {
                    *memo = Some(Checkpoint {
                        line: LineNo(line),
                        offset: ByteOffset(pos + line_start as u64),
                    });
                    *held = first.map(|l| {
                        (
                            Checkpoint {
                                line: l,
                                offset: ByteOffset(pos),
                            },
                            end,
                        )
                    });
                    return Ok(());
                }
            }
            if line_start == 0 && end == cap {
                if line >= range.start.0 {
                    // A line that fills the buffer exactly may still be
                    // the file's unterminated last one.
                    if read_retrying(&self.source, &mut [0u8; 1], ByteOffset(pos + end as u64))? > 0
                    {
                        return Err(Error::LineTooLong {
                            line: LineNo(line),
                            cap: Bytes(cap as u64),
                        });
                    }
                    buf.push(&scratch[..end])?;
                    return ended(line + 1, range.end);
                }
                // A long line we only skip: drop its bytes and keep going.
                pos += end as u64;
                filled = 0;
                first = None;
            } else {
                scratch.copy_within(line_start..end, 0);
                filled = end - line_start;
                pos += line_start as u64;
                first = Some(LineNo(line));
            }
        }
    }

    fn read_line_prefix(&self, line: LineNo, max: Bytes, buf: &mut LineBuf) -> Result<Bytes> {
        let mut scratch = self.scratch.lock().unwrap_or_else(PoisonError::into_inner);
        scratch.held = None;
        let start = self.line_start(line, &mut scratch)?;
        let scratch = &mut scratch.buf;
        buf.clear();
        buf.push(&[])?;
        // Bytes of the line seen so far; the first min(len, max) are kept.
        let mut len = 0u64;
        loop {
            let n = read_retrying(&self.source, scratch, ByteOffset(start + len))?;
            if n == 0 {
                if len == 0 {
                    return ended(line.0, line + 1).map(|()| Bytes(0));
                }
                return Ok(Bytes(len));
            }
            let (part, done) = match memchr::memchr(b'\n', &scratch[..n]) {
                Some(nl) => (nl + 1, true),
                None => (n, false),
            };
            let room = usize::try_from(max.0.saturating_sub(len)).unwrap_or(usize::MAX);
            let keep = room.min(part);
            if keep > 0 {
                buf.extend_last(&scratch[..keep])?;
            }
            len += part as u64;
            if done {
                return Ok(Bytes(len));
            }
        }
    }

    fn read_line_part(
        &self,
        line: LineNo,
        from: Bytes,
        max: Bytes,
        buf: &mut LineBuf,
    ) -> Result<bool> {
        let mut scratch = self.scratch.lock().unwrap_or_else(PoisonError::into_inner);
        scratch.held = None;
        let start = self.line_start(line, &mut scratch)?;
        buf.clear();
        buf.push(&[])?;
        if start >= self.source.len().0 {
            return ended(line.0, line + 1).map(|()| true);
        }
        self.line_part(start, from.0, max.0, &mut scratch.buf, buf)
    }
}

impl<S: Source, I: Borrow<LineIndex>> IndexedLines<S, I> {
    /// Tells the reader that `line` starts at byte `offset`, so reads at
    /// or after `line`, or a few lines before it, start there whenever that
    /// is closer than what the reader knows. The caller vouches for the
    /// pair: the skeleton knows where each segment starts from its
    /// anchors.
    pub fn seek(&self, line: LineNo, offset: ByteOffset) {
        debug_assert!(line.0 <= self.index.borrow().line_count());
        let mut scratch = self.scratch.lock().unwrap_or_else(PoisonError::into_inner);
        scratch.hint = Some(Checkpoint { line, offset });
    }

    /// Where `read_lines` starts scanning for `line`, and how many bytes
    /// at the front of the buffer, kept from the last read, start there.
    fn read_start(&self, scratch: &mut Scratch, line: LineNo) -> Result<(Checkpoint, usize)> {
        let index = self.index.borrow();
        let cp = index
            .checkpoint_before(line)
            .expect("line < line_count, so a checkpoint at or before it exists");
        if let Some(start) = scratch.held_start(cp, line)
            && let Some((first, len)) = scratch.held.take()
        {
            let skip = Bytes(start.offset.0 - first.offset.0).as_usize();
            scratch.buf.copy_within(skip..len, 0);
            return Ok((start, len - skip));
        }
        scratch.held = None;
        let start = scan_start(
            &self.source,
            index,
            [scratch.memo, scratch.hint],
            line,
            &mut scratch.buf,
        )?
        .expect("line < line_count, so a checkpoint at or before it exists");
        Ok((start, 0))
    }

    /// Reads up to `max` bytes of the line starting at file offset `start`,
    /// from `from` bytes into it, into `buf` through `scratch`. True when
    /// the piece ends the line: it holds the terminator, or the file ends.
    fn line_part(
        &self,
        start: u64,
        from: u64,
        max: u64,
        scratch: &mut [u8],
        buf: &mut LineBuf,
    ) -> Result<bool> {
        let end = self.source.len().0;
        let mut at = start + from;
        let mut left = max;
        while left > 0 {
            let want = Bytes(left.min(scratch.len() as u64)).as_usize();
            let n = read_retrying(&self.source, &mut scratch[..want], ByteOffset(at))?;
            if n == 0 {
                return Ok(true);
            }
            let (part, done) = match memchr::memchr(b'\n', &scratch[..n]) {
                Some(nl) => (nl + 1, true),
                None => (n, false),
            };
            buf.extend_last(&scratch[..part])?;
            if done {
                return Ok(true);
            }
            at += part as u64;
            left -= part as u64;
        }
        Ok(at >= end)
    }

    /// Byte offset where `line` starts: from the checkpoint before it, scan
    /// forward through `scratch`, counting line ends without keeping bytes,
    /// and remember it.
    fn line_start(&self, line: LineNo, scratch: &mut Scratch) -> Result<u64> {
        let index = self.index.borrow();
        let count = index.line_count();
        let start = scan_start(
            &self.source,
            index,
            [scratch.memo, scratch.hint],
            line,
            &mut scratch.buf,
        )?
        .ok_or(Error::LineOutOfRange { line, count })?;
        let found = Self::scan_to(&self.source, start, line, &mut scratch.buf)?;
        scratch.memo = Some(Checkpoint {
            line,
            offset: ByteOffset(found),
        });
        Ok(found)
    }

    /// Byte offset of `line`, scanning forward from the line start `start`.
    fn scan_to(source: &S, start: Checkpoint, line: LineNo, scratch: &mut [u8]) -> Result<u64> {
        let mut at = start.line.0;
        let mut pos = start.offset.0;
        while at < line.0 {
            let n = read_retrying(source, scratch, ByteOffset(pos))?;
            if n == 0 {
                return ended(at, line + 1).map(|()| pos);
            }
            let mut consumed = n;
            for nl in memchr_iter(b'\n', &scratch[..n]) {
                at += 1;
                if at == line.0 {
                    consumed = nl + 1;
                    break;
                }
            }
            pos += consumed as u64;
        }
        Ok(pos)
    }
}

/// The source ended at `line`; fine only if the request was satisfied.
fn ended(line: u64, end: LineNo) -> Result<()> {
    if line >= end.0 {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "source ended before the indexed line count",
        )
        .into())
    }
}

/// `read_at` that retries `Interrupted`, as `read_exact_at` does.
pub(crate) fn read_retrying<S: Source + ?Sized>(
    source: &S,
    buf: &mut [u8],
    offset: ByteOffset,
) -> io::Result<usize> {
    loop {
        match source.read_at(buf, offset) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            other => return other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::MemLines;

    /// An index with a checkpoint every `interval` lines, built by hand.
    fn index_of(data: &[u8], interval: u64) -> LineIndex {
        let mem = MemLines::new(data);
        let mut checkpoints = Vec::new();
        let mut offset = 0u64;
        for i in 0..mem.line_count() {
            if i % interval == 0 {
                checkpoints.push(Checkpoint {
                    line: LineNo(i),
                    offset: ByteOffset(offset),
                });
            }
            offset += mem.line(i).unwrap().len() as u64;
        }
        LineIndex::new(checkpoints, mem.line_count(), interval)
    }

    fn lines(data: &[u8], interval: u64, scratch: u64, range: Range<u64>) -> Result<Vec<Vec<u8>>> {
        let index = index_of(data, interval);
        let budget = Budget::new(Bytes::mib(1));
        let access = IndexedLines::new(data, &index, &budget, Half::Refine, Bytes(scratch))?;
        let mut buf = LineBuf::new();
        access.read_lines(LineNo(range.start)..LineNo(range.end), &mut buf)?;
        Ok(buf.iter().map(<[u8]>::to_vec).collect())
    }

    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn line_prefix_matches_memory_for_every_cap_and_scratch() {
        let long = vec![b'x'; 5000];
        let mut data = b"a\n\nbc\r\n".to_vec();
        data.extend_from_slice(&long);
        data.extend_from_slice(b"\nshort\n");
        data.extend_from_slice(&long);
        data.extend_from_slice(b"tail");
        let mem = MemLines::new(&data);
        let budget = Budget::new(Bytes::mib(1));
        for interval in [1, 2, 3, 100] {
            let index = index_of(&data, interval);
            for scratch in [1u64, 3, 64, 4096, 20_000] {
                let access =
                    IndexedLines::new(&data[..], &index, &budget, Half::Refine, Bytes(scratch))
                        .unwrap();
                for max in [0u64, 1, 2, 7, 4999, 5001, 1 << 20] {
                    for i in 0..mem.line_count() {
                        let (mut got, mut want) = (LineBuf::new(), LineBuf::new());
                        let len = access
                            .read_line_prefix(LineNo(i), Bytes(max), &mut got)
                            .unwrap();
                        let want_len = mem
                            .read_line_prefix(LineNo(i), Bytes(max), &mut want)
                            .unwrap();
                        assert_eq!(
                            len, want_len,
                            "line {i} interval {interval} scratch {scratch} max {max}"
                        );
                        assert_eq!(
                            got.line(0),
                            want.line(0),
                            "line {i} interval {interval} scratch {scratch} max {max}"
                        );
                    }
                }
                let mut buf = LineBuf::new();
                assert!(matches!(
                    access.read_line_prefix(LineNo(mem.line_count()), Bytes(4), &mut buf),
                    Err(Error::LineOutOfRange { .. })
                ));
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn line_prefix_holds_only_max_bytes_of_a_line_larger_than_the_budget() {
        // A 4 MiB line under a 64 KiB budget: read_lines cannot return it,
        // the prefix read can.
        let mut data = vec![b'y'; 4 << 20];
        data.push(b'\n');
        data.extend_from_slice(b"next\n");
        let index = index_of(&data, 1);
        let budget = Budget::new(Bytes(128 << 10));
        let access =
            IndexedLines::new(&data[..], &index, &budget, Half::Refine, Bytes(16 << 10)).unwrap();
        let mut buf = LineBuf::new();
        assert!(matches!(
            access.read_lines(LineNo(0)..LineNo(1), &mut buf),
            Err(Error::LineTooLong { .. })
        ));
        let len = access
            .read_line_prefix(LineNo(0), Bytes(200), &mut buf)
            .unwrap();
        assert_eq!(len, Bytes((4 << 20) + 1));
        assert_eq!(buf.line(0), &data[..200]);
        assert_eq!(
            access
                .read_line_prefix(LineNo(1), Bytes(200), &mut buf)
                .unwrap(),
            Bytes(5)
        );
        assert_eq!(buf.line(0), b"next\n");
    }

    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn line_parts_match_memory_for_every_piece_size_and_scratch() {
        let mut data = b"a\n\nbc\r\n".to_vec();
        data.extend_from_slice(&[b'x'; 300]);
        data.extend_from_slice(b"\nshort\n");
        data.extend_from_slice(&[b'y'; 77]);
        let mem = MemLines::new(&data);
        let budget = Budget::new(Bytes::mib(1));
        for interval in [1, 2, 100] {
            let index = index_of(&data, interval);
            for scratch in [1u64, 3, 64, 4096] {
                let access =
                    IndexedLines::new(&data[..], &index, &budget, Half::Refine, Bytes(scratch))
                        .unwrap();
                for max in [1u64, 2, 7, 100, 1 << 20] {
                    for i in 0..mem.line_count() {
                        let mut got = Vec::new();
                        let mut buf = LineBuf::new();
                        let mut from = 0;
                        loop {
                            let done = access
                                .read_line_part(LineNo(i), Bytes(from), Bytes(max), &mut buf)
                                .unwrap();
                            let piece = buf.line(0);
                            assert!(piece.len() as u64 <= max);
                            got.extend_from_slice(piece);
                            from += piece.len() as u64;
                            if done {
                                break;
                            }
                            assert_eq!(piece.len() as u64, max, "short piece before the end");
                        }
                        assert_eq!(
                            got,
                            mem.line(i).unwrap(),
                            "line {i} interval {interval} scratch {scratch} max {max}"
                        );
                    }
                }
                let mut buf = LineBuf::new();
                assert!(matches!(
                    access.read_line_part(LineNo(mem.line_count()), Bytes(0), Bytes(4), &mut buf),
                    Err(Error::LineOutOfRange { .. })
                ));
            }
        }
    }

    #[test]
    fn checkpoint_before_picks_the_floor() {
        let index = index_of(b"a\nb\nc\nd\ne", 2);
        assert_eq!(index.line_count(), 5);
        assert_eq!(index.checkpoint_before(LineNo(0)).unwrap().line, LineNo(0));
        assert_eq!(index.checkpoint_before(LineNo(3)).unwrap().line, LineNo(2));
        assert_eq!(
            index.checkpoint_before(LineNo(4)).unwrap().offset,
            ByteOffset(8)
        );
        assert_eq!(index.checkpoint_before(LineNo(5)), None);
        assert_eq!(
            LineIndex::new(Vec::new(), 0, 1).checkpoint_before(LineNo(0)),
            None
        );
    }

    #[test]
    fn reads_match_mem_lines_for_every_range_and_tiny_scratch() {
        let data = b"one\r\ntwo\n\nfour\nfive";
        let mem = MemLines::new(data);
        for interval in [1, 2, 3, 64] {
            for scratch in [6, 7, 64] {
                for start in 0..=5u64 {
                    for end in start..=5 {
                        let got = lines(data, interval, scratch, start..end).unwrap();
                        let want: Vec<Vec<u8>> = (start..end)
                            .map(|i| mem.line(i).unwrap().to_vec())
                            .collect();
                        assert_eq!(
                            got, want,
                            "interval {interval} scratch {scratch} {start}..{end}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn one_reader_serves_any_order_of_reads() {
        let data: Vec<u8> = (0..50)
            .flat_map(|i| format!("{}\n", "z".repeat(i % 7)).into_bytes())
            .collect();
        let mem = MemLines::new(&data);
        let budget = Budget::new(Bytes::mib(1));
        let index = index_of(&data, 16);
        let access =
            IndexedLines::new(&data[..], &index, &budget, Half::Refine, Bytes(64)).unwrap();
        let mut buf = LineBuf::new();
        // Forward in small steps, then backward, then prefixes in between.
        let ranges = (0..50)
            .map(|i| i..i + 1)
            .chain((0..49).rev().map(|i| i..i + 2));
        for r in ranges {
            access
                .read_lines(LineNo(r.start)..LineNo(r.end), &mut buf)
                .unwrap();
            for (k, line) in buf.iter().enumerate() {
                assert_eq!(line, mem.line(r.start + k as u64).unwrap(), "{r:?}");
            }
            let len = access
                .read_line_prefix(LineNo(r.end - 1), Bytes(3), &mut buf)
                .unwrap();
            assert_eq!(len.0, mem.line(r.end - 1).unwrap().len() as u64);
        }
    }

    /// A source that counts its reads.
    struct Counted<'d>(&'d [u8], std::sync::atomic::AtomicU64);

    impl Source for Counted<'_> {
        fn len(&self) -> Bytes {
            Source::len(self.0)
        }

        fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.0.read_at(buf, offset)
        }
    }

    #[test]
    fn reads_inside_the_last_buffer_do_not_touch_the_source() {
        let data: Vec<u8> = (0..400)
            .flat_map(|i| format!("{i}\n").into_bytes())
            .collect();
        let mem = MemLines::new(&data);
        let index = index_of(&data, 64);
        let budget = Budget::new(Bytes::mib(1));
        let src = Counted(&data, 0.into());
        let access = IndexedLines::new(&src, &index, &budget, Half::Refine, Bytes(256)).unwrap();
        let mut buf = LineBuf::new();
        let reads = || src.1.load(std::sync::atomic::Ordering::Relaxed);
        access.read_lines(LineNo(10)..LineNo(12), &mut buf).unwrap();
        let first = reads();
        // The same lines, the next ones and a prefix that starts inside
        // the buffer cost nothing; a prefix read empties the buffer.
        for r in [10..12, 12..20, 14..30] {
            access
                .read_lines(LineNo(r.start)..LineNo(r.end), &mut buf)
                .unwrap();
            for (k, line) in buf.iter().enumerate() {
                assert_eq!(line, mem.line(r.start + k as u64).unwrap(), "{r:?}");
            }
        }
        assert_eq!(reads(), first);
        access
            .read_line_prefix(LineNo(31), Bytes(8), &mut buf)
            .unwrap();
        let after = reads();
        access.read_lines(LineNo(31)..LineNo(33), &mut buf).unwrap();
        assert!(reads() > after);
        assert_eq!(buf.iter().next().unwrap(), mem.line(31).unwrap());
    }

    proptest::proptest! {
        #[test]
        fn any_sequence_of_reads_matches_memory(
            lens in proptest::collection::vec(0usize..40, 1..120),
            reads in proptest::collection::vec((0usize..130, 0usize..12, 0u8..3), 1..40),
            scratch in 48u64..200,
            interval in 1u64..40,
        ) {
            let data: Vec<u8> = lens
                .iter()
                .enumerate()
                .flat_map(|(i, &n)| {
                    let mut l = vec![b"abcdefghijklmnopqrstuvwxyz"[i % 26]; n];
                    l.push(b'\n');
                    l
                })
                .collect();
            let mem = MemLines::new(&data);
            let n = mem.line_count();
            let index = index_of(&data, interval);
            let budget = Budget::new(Bytes::mib(1));
            let access =
                IndexedLines::new(&data[..], &index, &budget, Half::Refine, Bytes(scratch)).unwrap();
            let mut buf = LineBuf::new();
            for (start, len, kind) in reads {
                let start = (start as u64).min(n - 1);
                let end = (start + len as u64 + 1).min(n);
                match kind {
                    0 => {
                        access.read_lines(LineNo(start)..LineNo(end), &mut buf).unwrap();
                        let want: Vec<&[u8]> = (start..end).map(|i| mem.line(i).unwrap()).collect();
                        let got: Vec<&[u8]> = buf.iter().collect();
                        proptest::prop_assert_eq!(got, want);
                    }
                    1 => {
                        let len = access.read_line_prefix(LineNo(start), Bytes(5), &mut buf).unwrap();
                        proptest::prop_assert_eq!(len.0, mem.line(start).unwrap().len() as u64);
                    }
                    _ => {
                        let at = mem.line(0).map_or(0, |_| (0..start).map(|i| mem.line(i).unwrap().len() as u64).sum());
                        access.seek(LineNo(start), ByteOffset(at));
                    }
                }
            }
        }
    }

    #[test]
    fn reads_just_before_a_sought_line_step_back_from_it() {
        let data: Vec<u8> = (0..300)
            .flat_map(|i| format!("{}\n", "w".repeat(i % 11)).into_bytes())
            .collect();
        let mem = MemLines::new(&data);
        let offset = |l: u64| ByteOffset((0..l).map(|i| mem.line(i).unwrap().len() as u64).sum());
        let index = index_of(&data, 1000);
        let budget = Budget::new(Bytes::mib(1));
        for scratch in [12u64, 64, 4096] {
            let access =
                IndexedLines::new(&data[..], &index, &budget, Half::Refine, Bytes(scratch))
                    .unwrap();
            let mut buf = LineBuf::new();
            for (at, from) in [
                (250u64, 247u64),
                (250, 234),
                (5, 0),
                (1, 0),
                (200, 199),
                (299, 290),
            ] {
                access.seek(LineNo(at), offset(at));
                access
                    .read_lines(LineNo(from)..LineNo(at + 1), &mut buf)
                    .unwrap();
                for (k, line) in buf.iter().enumerate() {
                    assert_eq!(
                        line,
                        mem.line(from + k as u64).unwrap(),
                        "scratch {scratch} {from}..={at}"
                    );
                }
                access.seek(LineNo(at), offset(at));
                let len = access
                    .read_line_prefix(LineNo(from), Bytes(64), &mut buf)
                    .unwrap();
                assert_eq!(len.0, mem.line(from).unwrap().len() as u64);
            }
        }
    }

    #[test]
    fn long_lines_are_skippable_but_not_readable() {
        let mut data = vec![b'x'; 100];
        data.extend_from_slice(b"\nshort\n");
        assert_eq!(lines(&data, 1, 16, 1..2).unwrap(), [b"short\n".to_vec()]);
        let exact = vec![b'x'; 16];
        assert_eq!(lines(&exact, 1, 16, 0..1).unwrap(), [exact.as_slice()]);
        assert_eq!(
            lines(&exact, 1, 15, 0..1).unwrap_err().to_string(),
            "line 0 exceeds buffer of 15 bytes"
        );
        assert!(matches!(
            lines(&data, 1, 16, 0..1),
            Err(Error::LineTooLong {
                line: LineNo(0),
                cap: Bytes(16)
            })
        ));
        assert!(matches!(
            lines(&data, 1, 16, 1..3),
            Err(Error::LineOutOfRange { .. })
        ));
    }

    #[test]
    fn scratch_is_charged_to_the_named_half_and_released() {
        let data: &[u8] = b"a\n";
        let index = index_of(data, 1);
        let budget = Budget::new(Bytes(64));
        let access = IndexedLines::new(data, &index, &budget, Half::Refine, Bytes(20)).unwrap();
        assert_eq!(budget.used(Half::Refine), Bytes(20));
        assert_eq!(budget.used(Half::Skeleton), Bytes(0));
        assert!(IndexedLines::new(data, &index, &budget, Half::Refine, Bytes(20)).is_err());
        drop(access);
        assert_eq!(budget.used(Half::Refine), Bytes(0));
    }
}
