//! Reading a segment forward through a caller's block, for verification.

use crate::error::Result;
use crate::index::read_retrying;
use crate::source::Source;
use crate::types::ByteOffset;

/// Lines read forward from a line start through `block`, handed out where
/// they were read: only the partial line at the block's end moves to its
/// front before the next read.
pub(super) struct Forward<'a, S: ?Sized> {
    source: &'a S,
    block: &'a mut [u8],
    /// File offset of `block[0]`.
    pos: u64,
    /// Where the next line starts in `block`.
    start: usize,
    /// Bytes from `start` up to here hold no newline.
    scanned: usize,
    filled: usize,
}

impl<'a, S: Source + ?Sized> Forward<'a, S> {
    /// A reader whose first line starts at `at`.
    pub(super) fn new(source: &'a S, at: ByteOffset, block: &'a mut [u8]) -> Self {
        Self {
            source,
            block,
            pos: at.0,
            start: 0,
            scanned: 0,
            filled: 0,
        }
    }

    /// Reads the next line and returns it, terminator included; `None`
    /// when it does not fit the block, and the reader stays at its start.
    fn next_line(&mut self) -> Result<Option<&[u8]>> {
        loop {
            if let Some(i) = memchr::memchr(b'\n', &self.block[self.scanned..self.filled]) {
                let (from, to) = (self.start, self.scanned + i + 1);
                (self.start, self.scanned) = (to, to);
                return Ok(Some(&self.block[from..to]));
            }
            self.scanned = self.filled;
            if self.start == 0 && self.filled == self.block.len() {
                return Ok(None);
            }
            self.block.copy_within(self.start..self.filled, 0);
            self.pos += self.start as u64;
            self.filled -= self.start;
            self.scanned -= self.start;
            self.start = 0;
            if self.read()? == 0 {
                if self.filled == 0 {
                    return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
                }
                // The file's last line, without a newline.
                let from = self.start;
                (self.start, self.scanned) = (self.filled, self.filled);
                return Ok(Some(&self.block[from..self.filled]));
            }
        }
    }

    /// Reads past the next `lines` lines, whatever their length.
    pub(super) fn skip(&mut self, lines: usize) -> Result<()> {
        for _ in 0..lines {
            if self.next_line()?.is_none() {
                self.skip_long()?;
            }
        }
        Ok(())
    }

    /// Reads past a line that fills the block from its front.
    fn skip_long(&mut self) -> Result<()> {
        loop {
            if let Some(i) = memchr::memchr(b'\n', &self.block[self.scanned..self.filled]) {
                self.start = self.scanned + i + 1;
                self.scanned = self.start;
                return Ok(());
            }
            self.restart(self.pos + self.filled as u64);
            if self.read()? == 0 {
                return Ok(());
            }
        }
    }

    /// Restarts the reader at file offset `at`, a line start.
    fn restart(&mut self, at: u64) {
        self.pos = at;
        (self.start, self.scanned, self.filled) = (0, 0, 0);
    }

    /// Appends what the source holds after the filled bytes; 0 at its end.
    fn read(&mut self) -> Result<usize> {
        let at = ByteOffset(self.pos + self.filled as u64);
        let n = read_retrying(self.source, &mut self.block[self.filled..], at)?;
        self.filled += n;
        Ok(n)
    }
}

/// Reads the next line of `a` and of `b` and returns whether their bytes
/// are equal. The blocks must have the same length: a line that fits one
/// and not the other is then longer, or lacks the newline the other has.
pub(super) fn same_next<S: Source + ?Sized, T: Source + ?Sized>(
    a: &mut Forward<'_, S>,
    b: &mut Forward<'_, T>,
) -> Result<bool> {
    debug_assert_eq!(a.block.len(), b.block.len(), "blocks of one length");
    let (line_a, line_b) = (a.next_line()?, b.next_line()?);
    if let (Some(line_a), Some(line_b)) = (line_a, line_b) {
        return Ok(line_a == line_b);
    }
    let (fits_a, fits_b) = (line_a.is_some(), line_b.is_some());
    if fits_a {
        b.skip_long()?;
        return Ok(false);
    }
    if fits_b {
        a.skip_long()?;
        return Ok(false);
    }
    // Both lines are longer than the blocks: compare them block by block,
    // then read past the rest of each from where the comparison stopped.
    let (at_a, at_b) = (a.pos, b.pos);
    let mut done = 0u64;
    let same = loop {
        // The first piece of each is in its block already.
        let (got_a, got_b) = if done == 0 {
            (a.block.len(), b.block.len())
        } else {
            (
                fill(a.source, a.block, at_a + done)?,
                fill(b.source, b.block, at_b + done)?,
            )
        };
        let end_a = line_end(&a.block[..got_a], got_a < a.block.len());
        let end_b = line_end(&b.block[..got_b], got_b < b.block.len());
        if end_a != end_b || a.block[..end_a.unwrap_or(got_a)] != b.block[..end_b.unwrap_or(got_b)]
        {
            break false;
        }
        if let Some(end) = end_a {
            done += end as u64;
            break true;
        }
        done += got_a as u64;
    };
    a.restart(at_a + done);
    b.restart(at_b + done);
    if !same {
        // `done` stops at the start of the piece that differed, still
        // inside both lines or at the end of the source.
        a.skip_long()?;
        b.skip_long()?;
    }
    Ok(same)
}

/// Where the line ends in `piece`, the next bytes of it: after its
/// newline, or at the piece's end when the source ended there.
fn line_end(piece: &[u8], at_end: bool) -> Option<usize> {
    memchr::memchr(b'\n', piece)
        .map(|i| i + 1)
        .or(at_end.then_some(piece.len()))
}

/// Fills `buf` from `at` and returns how many bytes it got, fewer only at
/// the source's end.
fn fill<S: Source + ?Sized>(source: &S, buf: &mut [u8], at: u64) -> Result<usize> {
    let left = source.len().0.saturating_sub(at);
    let got = usize::try_from(left).map_or(buf.len(), |left| left.min(buf.len()));
    source.read_exact_at(&mut buf[..got], ByteOffset(at))?;
    Ok(got)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(data: &[u8]) -> Vec<&[u8]> {
        data.split_inclusive(|&c| c == b'\n').collect()
    }

    /// Whether a line comes back whole through a block of `len` bytes.
    fn fits(line: &[u8], len: usize) -> bool {
        line.len() < len || (line.len() == len && line.ends_with(b"\n"))
    }

    #[test]
    fn lines_come_back_whole_through_every_block_length() {
        let data = b"a\nbb\n\nccccccc\nd\neeeeeeeeeeee\nf\ngg";
        for len in 1..=16 {
            let mut block = vec![0; len];
            let mut r = Forward::new(&data[..], ByteOffset(0), &mut block);
            for (k, want) in lines(data).into_iter().enumerate() {
                if fits(want, len) {
                    let got = r.next_line().unwrap();
                    assert_eq!(got, Some(want), "line {k}, block {len}");
                } else {
                    assert_eq!(r.next_line().unwrap(), None, "line {k}, block {len}");
                    r.skip_long().unwrap();
                }
            }
            assert!(r.next_line().is_err(), "nothing after the last line");
        }
    }

    #[test]
    fn skipping_lands_on_the_next_line_for_every_block_length() {
        let data = b"one\ntwo two two\n3\n\nfour four\nfive";
        for len in 1..=12 {
            for (skip, want) in lines(data).into_iter().enumerate() {
                let mut block = vec![0; len];
                let mut r = Forward::new(&data[..], ByteOffset(0), &mut block);
                r.skip(skip).unwrap();
                if fits(want, len) {
                    assert_eq!(
                        r.next_line().unwrap(),
                        Some(want),
                        "skip {skip}, block {len}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_reader_starts_at_the_offset_given() {
        let data = b"skipped\nfirst\nsecond\n";
        let mut block = [0; 8];
        let mut r = Forward::new(&data[..], ByteOffset(8), &mut block);
        assert_eq!(r.next_line().unwrap(), Some(&b"first\n"[..]));
        assert_eq!(r.next_line().unwrap(), Some(&b"second\n"[..]));
    }

    /// Every pair of lines that differ near a block edge, by a byte, by
    /// length or by the newline, in either order, with A and B shifted
    /// against each other so their blocks split lines in other places.
    #[test]
    fn lines_compare_equal_exactly_when_their_bytes_are() {
        let shapes: Vec<Vec<u8>> = (0..=10)
            .flat_map(|n| {
                let body = vec![b'x'; n];
                let mut changed = body.clone();
                if let Some(c) = changed.last_mut() {
                    *c = b'y';
                }
                [body.clone(), changed]
            })
            .collect();
        let mut cases: Vec<(Vec<u8>, bool)> = Vec::new();
        for s in &shapes {
            let mut l = s.clone();
            l.push(b'\n');
            cases.push((l, true));
            if !s.is_empty() {
                cases.push((s.clone(), false));
            }
        }
        for len in 1..=12 {
            for (x, x_nl) in &cases {
                for (y, y_nl) in &cases {
                    for shift in 0..3 {
                        let mut a = b"p\n".to_vec();
                        a.extend_from_slice(x);
                        let mut b = vec![b'q'; shift];
                        b.push(b'\n');
                        b.extend_from_slice(y);
                        if *x_nl {
                            a.extend_from_slice(b"next\n");
                        }
                        if *y_nl {
                            b.extend_from_slice(b"next\n");
                        }
                        let (mut ba, mut bb) = (vec![0; len], vec![0; len]);
                        let mut ra = Forward::new(&a[..], ByteOffset(0), &mut ba);
                        let mut rb = Forward::new(&b[..], ByteOffset(0), &mut bb);
                        ra.skip(1).unwrap();
                        rb.skip(1).unwrap();
                        let same = same_next(&mut ra, &mut rb).unwrap();
                        assert_eq!(same, x == y, "{x:?} against {y:?}, block {len}");
                        if *x_nl && fits(b"next\n", len) {
                            assert_eq!(ra.next_line().unwrap(), Some(&b"next\n"[..]));
                        }
                        if *y_nl && fits(b"next\n", len) {
                            assert_eq!(rb.next_line().unwrap(), Some(&b"next\n"[..]));
                        }
                    }
                }
            }
        }
    }
}
