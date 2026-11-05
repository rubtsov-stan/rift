//! Sparse line index: a checkpoint offset at least every C lines.
//!
//! The scanner records one checkpoint per `interval` lines inside each of
//! its chunks, so consecutive checkpoints are at most `interval` lines apart
//! and a chunk boundary only brings them closer. [`IndexedLines`] turns the
//! index plus a [`Source`] into [`LineAccess`]: reaching any line costs one
//! binary search and a forward scan of fewer than `interval` lines.

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
/// thread its own instance when that matters.
#[derive(Debug)]
pub struct IndexedLines<'a, S: ?Sized> {
    source: &'a S,
    index: &'a LineIndex,
    scratch: Mutex<Vec<u8>>,
    _reservation: Reservation,
}

impl<'a, S: Source + ?Sized> IndexedLines<'a, S> {
    /// Reserves a `scratch`-byte read buffer from `half`.
    ///
    /// A requested line longer than `scratch` is reported as
    /// [`Error::LineTooLong`]; lines that are only skipped over may be any
    /// length.
    ///
    /// # Errors
    ///
    /// [`Error::Budget`] if `half` cannot hold `scratch` bytes.
    pub fn new(
        source: &'a S,
        index: &'a LineIndex,
        budget: &Budget,
        half: Half,
        scratch: Bytes,
    ) -> Result<Self> {
        let reservation = budget.try_reserve(half, scratch)?;
        Ok(Self {
            source,
            index,
            scratch: Mutex::new(vec![0; scratch.as_usize()]),
            _reservation: reservation,
        })
    }
}

impl<S: Source + ?Sized> LineAccess for IndexedLines<'_, S> {
    fn line_count(&self) -> u64 {
        self.index.line_count()
    }

    fn read_lines(&self, range: Range<LineNo>, buf: &mut LineBuf) -> Result<()> {
        let count = self.index.line_count();
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
        let start = self
            .index
            .checkpoint_before(range.start)
            .expect("range.start < line_count, so a checkpoint at or before it exists");
        let mut scratch = self.scratch.lock().unwrap_or_else(PoisonError::into_inner);
        let cap = scratch.len();
        let mut line = start.line.0;
        let mut pos = start.offset.0;
        let mut filled = 0usize;
        loop {
            let n = read_retrying(
                self.source,
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
            let end = filled + n;
            let mut line_start = 0usize;
            for nl in memchr_iter(b'\n', &scratch[filled..end]).map(|i| i + filled) {
                if line >= range.start.0 {
                    buf.push(&scratch[line_start..=nl])?;
                }
                line += 1;
                line_start = nl + 1;
                if line == range.end.0 {
                    return Ok(());
                }
            }
            if line_start == 0 && end == cap {
                if line >= range.start.0 {
                    // A line that fills the buffer exactly may still be
                    // the file's unterminated last one.
                    if read_retrying(self.source, &mut [0u8; 1], ByteOffset(pos + end as u64))? > 0
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
            } else {
                scratch.copy_within(line_start..end, 0);
                filled = end - line_start;
                pos += line_start as u64;
            }
        }
    }

    fn read_line_prefix(&self, line: LineNo, max: Bytes, buf: &mut LineBuf) -> Result<Bytes> {
        let mut scratch = self.scratch.lock().unwrap_or_else(PoisonError::into_inner);
        let start = self.line_start(line, &mut scratch)?;
        buf.clear();
        buf.push(&[])?;
        // Bytes of the line seen so far; the first min(len, max) are kept.
        let mut len = 0u64;
        loop {
            let n = read_retrying(self.source, &mut scratch, ByteOffset(start + len))?;
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
}

impl<S: Source + ?Sized> IndexedLines<'_, S> {
    /// Byte offset where `line` starts: from the checkpoint before it, scan
    /// forward through `scratch`, counting line ends without keeping bytes.
    fn line_start(&self, line: LineNo, scratch: &mut [u8]) -> Result<u64> {
        let count = self.index.line_count();
        let start = self
            .index
            .checkpoint_before(line)
            .ok_or(Error::LineOutOfRange { line, count })?;
        let mut at = start.line.0;
        let mut pos = start.offset.0;
        while at < line.0 {
            let n = read_retrying(self.source, scratch, ByteOffset(pos))?;
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
