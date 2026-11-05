//! Line access: read a range of lines into a caller-owned buffer.

use std::ops::Range;

use crate::error::{Error, Result};
use crate::types::LineNo;

/// A reusable buffer holding the bytes of consecutive lines.
///
/// Lines keep their terminator (`\n` or `\r\n`); the last line of a file may
/// have none. Both vectors are reused across calls, so reading lines in a
/// loop allocates nothing after the first iteration.
#[derive(Debug, Default)]
pub struct LineBuf {
    bytes: Vec<u8>,
    ends: Vec<usize>,
}

impl LineBuf {
    /// An empty buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets the lines but keeps the allocations.
    pub fn clear(&mut self) {
        self.bytes.clear();
        self.ends.clear();
    }

    /// Appends one line. Grows through `try_reserve`, never panics on OOM.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] with kind `OutOfMemory` when the buffer cannot
    /// grow.
    pub fn push(&mut self, line: &[u8]) -> Result<()> {
        self.bytes.try_reserve(line.len()).map_err(oom)?;
        self.ends.try_reserve(1).map_err(oom)?;
        self.bytes.extend_from_slice(line);
        self.ends.push(self.bytes.len());
        Ok(())
    }

    /// Appends several lines stored back to back in `bytes`, where `ends`
    /// gives each line's end offset within `bytes` in increasing order, the
    /// last one equal to `bytes.len()`. One reservation per vector instead
    /// of one per line.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Io`] with kind `OutOfMemory` when the buffer cannot
    /// grow.
    pub fn push_lines(
        &mut self,
        bytes: &[u8],
        ends: impl ExactSizeIterator<Item = usize>,
    ) -> Result<()> {
        self.bytes.try_reserve(bytes.len()).map_err(oom)?;
        self.ends.try_reserve(ends.len()).map_err(oom)?;
        let base = self.bytes.len();
        self.bytes.extend_from_slice(bytes);
        self.ends.extend(ends.map(|end| base + end));
        Ok(())
    }

    /// Number of lines held.
    #[must_use]
    pub fn len(&self) -> usize {
        self.ends.len()
    }

    /// Whether no lines are held.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.ends.is_empty()
    }

    /// The `i`-th line held, terminator included.
    ///
    /// # Panics
    ///
    /// Panics if `i >= len()`.
    #[must_use]
    pub fn line(&self, i: usize) -> &[u8] {
        let start = if i == 0 { 0 } else { self.ends[i - 1] };
        &self.bytes[start..self.ends[i]]
    }

    /// All lines held, in order.
    #[must_use]
    pub fn iter(&self) -> impl ExactSizeIterator<Item = &[u8]> {
        (0..self.len()).map(|i| self.line(i))
    }
}

fn oom(_: std::collections::TryReserveError) -> Error {
    std::io::Error::from(std::io::ErrorKind::OutOfMemory).into()
}

/// Random access to lines by number.
pub trait LineAccess: Sync {
    /// Lines in the file.
    fn line_count(&self) -> u64;

    /// Replaces the contents of `buf` with lines `range`, in order.
    ///
    /// # Errors
    ///
    /// `LineOutOfRange` if `range.end` exceeds the line count, `Io` on a read
    /// failure.
    fn read_lines(&self, range: Range<LineNo>, buf: &mut LineBuf) -> Result<()>;
}

impl<L: LineAccess + ?Sized> LineAccess for &L {
    fn line_count(&self) -> u64 {
        (**self).line_count()
    }

    fn read_lines(&self, range: Range<LineNo>, buf: &mut LineBuf) -> Result<()> {
        (**self).read_lines(range, buf)
    }
}

/// In-memory lines with precomputed starts, for tests and small inputs.
#[derive(Debug)]
pub struct MemLines<'a> {
    data: &'a [u8],
    /// Byte offset where each line begins, plus a final entry at `data.len()`.
    starts: Vec<usize>,
}

impl<'a> MemLines<'a> {
    /// Indexes `data`. A trailing line without `\n` counts as a line; an
    /// empty input has no lines.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        let mut starts = Vec::with_capacity(data.len() / 32 + 2);
        starts.push(0);
        for nl in memchr::memchr_iter(b'\n', data) {
            starts.push(nl + 1);
        }
        if starts.last() != Some(&data.len()) {
            starts.push(data.len());
        }
        Self { data, starts }
    }

    /// The bytes of line `i`, terminator included.
    #[must_use]
    pub fn line(&self, i: u64) -> Option<&'a [u8]> {
        let i = usize::try_from(i).ok()?;
        if i + 1 >= self.starts.len() {
            return None;
        }
        Some(&self.data[self.starts[i]..self.starts[i + 1]])
    }
}

impl LineAccess for MemLines<'_> {
    fn line_count(&self) -> u64 {
        (self.starts.len() - 1) as u64
    }

    fn read_lines(&self, range: Range<LineNo>, buf: &mut LineBuf) -> Result<()> {
        let count = self.line_count();
        if range.end.0 > count {
            return Err(Error::LineOutOfRange {
                line: range.end,
                count,
            });
        }
        buf.clear();
        let (first, last) = (
            usize::try_from(range.start.0).expect("below line_count"),
            usize::try_from(range.end.0).expect("at most line_count"),
        );
        if first >= last {
            return Ok(());
        }
        let base = self.starts[first];
        let bytes = &self.data[base..self.starts[last]];
        buf.push_lines(
            bytes,
            self.starts[first + 1..=last].iter().map(|&s| s - base),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines_of(data: &[u8]) -> Vec<Vec<u8>> {
        let m = MemLines::new(data);
        let mut buf = LineBuf::new();
        m.read_lines(LineNo(0)..LineNo(m.line_count()), &mut buf)
            .unwrap();
        buf.iter().map(<[u8]>::to_vec).collect()
    }

    #[test]
    fn counts_follow_wc_plus_unterminated_tail() {
        assert_eq!(MemLines::new(b"").line_count(), 0);
        assert_eq!(MemLines::new(b"\n").line_count(), 1);
        assert_eq!(MemLines::new(b"a\nb\n").line_count(), 2);
        assert_eq!(MemLines::new(b"a\nb").line_count(), 2);
        assert_eq!(MemLines::new(b"a\r\nb\r\n").line_count(), 2);
    }

    #[test]
    fn lines_keep_their_terminators() {
        assert_eq!(
            lines_of(b"a\r\nb\nc"),
            [b"a\r\n".to_vec(), b"b\n".to_vec(), b"c".to_vec()]
        );
        assert_eq!(lines_of(b"\n\n"), [b"\n".to_vec(), b"\n".to_vec()]);
    }

    #[test]
    fn push_lines_matches_repeated_push() {
        let mut one = LineBuf::new();
        let mut many = LineBuf::new();
        one.push(b"a\n").unwrap();
        one.push(b"bc\r\n").unwrap();
        one.push(b"d").unwrap();
        many.push_lines(b"a\nbc\r\nd", [2usize, 6, 7].into_iter())
            .unwrap();
        assert_eq!(
            one.iter().collect::<Vec<_>>(),
            many.iter().collect::<Vec<_>>()
        );
        many.push_lines(b"", [].into_iter()).unwrap();
        assert_eq!(many.len(), 3);
    }

    #[test]
    fn out_of_range_is_an_error_and_buffer_is_reused() {
        let m = MemLines::new(b"x\ny\n");
        let mut buf = LineBuf::new();
        assert!(matches!(
            m.read_lines(LineNo(1)..LineNo(3), &mut buf),
            Err(Error::LineOutOfRange { .. })
        ));
        m.read_lines(LineNo(1)..LineNo(2), &mut buf).unwrap();
        assert_eq!(buf.len(), 1);
        assert_eq!(buf.line(0), b"y\n");
        m.read_lines(LineNo(0)..LineNo(0), &mut buf).unwrap();
        assert!(buf.is_empty());
    }
}
