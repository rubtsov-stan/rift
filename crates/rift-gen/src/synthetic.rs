//! A pair of files generated on demand, for memory tests on inputs larger
//! than the disk.
//!
//! [`Synthetic`] is a [`Source`] whose bytes are computed from the seed and
//! the position, so a `pread` at any offset works without a file. The file
//! is cut into blocks of [`BLOCK`] bytes, the same size in A and B. Each
//! block holds whole lines derived from the seed and the block index, and
//! ends with a padding line that fills it exactly; B's block applies
//! mutations to the same lines (a changed line, a deleted one, an inserted
//! one, each with probability `change` per line) and its padding line
//! absorbs the difference in length. Finding the block of an offset is a
//! division.
//!
//! A read generates its block from the start up to the bytes asked for,
//! one line at a time into a buffer on the stack, so the source holds no
//! heap at all: its bookkeeping is the struct itself, [`Synthetic`]'s
//! `size_of`, and it does not count against the heap of the diff that
//! reads it.

use std::io::{self, Write};

use rift_core::{ByteOffset, Bytes, Source};

use crate::rng::Rng;

/// Bytes per block, in both files.
pub const BLOCK: u64 = 16 << 10;
/// Room left in every block of A for B's padding to absorb insertions.
const SLACK: u64 = 512;
/// The longest line the generator writes.
const MAX_LINE: usize = 160;

/// Which file of the pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The original.
    A,
    /// The original with mutations.
    B,
}

/// One file of a generated pair. See the module documentation.
#[derive(Debug, Clone, Copy)]
pub struct Synthetic {
    seed: u64,
    blocks: u64,
    change: f64,
    side: Side,
}

impl Synthetic {
    /// File `file` of a pair of about `size` bytes each, rounded up to whole
    /// blocks, where each line of A is changed in B with probability
    /// `change`.
    #[must_use]
    pub fn new(seed: u64, size: u64, change: f64, file: Side) -> Self {
        Self {
            seed,
            blocks: size.div_ceil(BLOCK).max(1),
            change,
            side: file,
        }
    }

    /// Calls `emit` with every line of `block`, in order, until it returns
    /// false.
    fn lines(&self, block: u64, mut emit: impl FnMut(&[u8]) -> bool) {
        let mix = |salt: u64| {
            let mut r = Rng::new(self.seed ^ salt ^ block.wrapping_mul(0x9E37_79B9_7F4A_7C15));
            Rng::new(r.next_u64())
        };
        // Line content and mutation decisions come from separate streams,
        // and both are consumed the same way on both sides, so A and B
        // agree on every line a mutation did not touch.
        let (mut content, mut mutation) = (mix(0), mix(0x5555_5555_5555_5555));
        let mut line = [0u8; MAX_LINE];
        let mut other = [0u8; MAX_LINE];
        // B never gets SLACK bytes ahead of A, so its lines never reach
        // the end of the block.
        let (mut total_a, mut total_b) = (0u64, 0u64);
        for j in 0.. {
            let id = block * 1000 + j;
            let len = row(&mut line, id, &mut content);
            if total_a + len as u64 > BLOCK - SLACK {
                break;
            }
            total_a += len as u64;
            let kind = mutation.chance(self.change).then(|| mutation.below(3));
            let alt = kind.map(|_| row(&mut other, id + 500, &mut mutation));
            let fits = |bytes: usize| total_b + (bytes as u64) < total_a + SLACK;
            let out: [&[u8]; 2] = match (self.side, kind, alt) {
                (Side::B, Some(0), _) => [&[], &[]],
                (Side::B, Some(1), Some(n)) if fits(n + len) => [&other[..n], &line[..len]],
                (Side::B, Some(2), Some(n)) if fits(n) => [&other[..n], &[]],
                _ => [&line[..len], &[]],
            };
            for part in out {
                total_b += part.len() as u64;
                if !part.is_empty() && !emit(part) {
                    return;
                }
            }
        }
        // The padding line fills the block exactly: at least SLACK bytes in
        // A, at least one in B.
        let used = if self.side == Side::A {
            total_a
        } else {
            total_b
        };
        let mut left = BLOCK - used;
        let dashes = [b'-'; MAX_LINE];
        while left > 1 {
            let n = usize::try_from(left - 1).unwrap_or(MAX_LINE).min(MAX_LINE);
            if !emit(&dashes[..n]) {
                return;
            }
            left -= n as u64;
        }
        emit(b"\n");
    }
}

/// Writes one row for `id` into `buf` and returns its length. Its shape
/// follows a SQL dump: unique id, a few words, a number.
fn row(buf: &mut [u8; MAX_LINE], id: u64, rng: &mut Rng) -> usize {
    let mut w = &mut buf[..];
    let _ = write!(w, "INSERT INTO t VALUES ({id}, '");
    for k in 0..rng.range(2, 6) {
        if k > 0 {
            let _ = w.write_all(b" ");
        }
        for _ in 0..rng.range(2, 9) {
            let _ = w.write_all(&[b'a' + rng.byte(26)]);
        }
    }
    let _ = writeln!(w, "', {});", rng.below(1_000_000));
    MAX_LINE - w.len()
}

impl Source for Synthetic {
    fn len(&self) -> Bytes {
        Bytes(self.blocks * BLOCK)
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize> {
        let len = self.blocks * BLOCK;
        if offset.0 >= len || buf.is_empty() {
            return Ok(0);
        }
        let (block, from) = (offset.0 / BLOCK, offset.0 % BLOCK);
        let n = usize::try_from(BLOCK - from).map_or(buf.len(), |rest| rest.min(buf.len()));
        let want = from..from + n as u64;
        let mut pos = 0u64;
        // A pad line is cut in pieces of at most MAX_LINE by `lines`, so
        // every piece is copied whole or in part like a line.
        self.lines(block, |part| {
            let end = pos + part.len() as u64;
            let (lo, hi) = (pos.max(want.start), end.min(want.end));
            if lo < hi {
                let dst = usize::try_from(lo - want.start).expect("within buf");
                let src = usize::try_from(lo - pos).expect("within part");
                let k = usize::try_from(hi - lo).expect("within part");
                buf[dst..dst + k].copy_from_slice(&part[src..src + k]);
            }
            pos = end;
            pos < want.end
        });
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn whole(s: &Synthetic) -> Vec<u8> {
        let mut v = vec![0u8; usize::try_from(s.len().0).unwrap()];
        s.read_exact_at(&mut v, ByteOffset(0)).unwrap();
        v
    }

    #[test]
    fn reads_at_any_offset_agree_with_the_whole() {
        let s = Synthetic::new(3, 5 * BLOCK, 0.01, Side::B);
        let all = whole(&s);
        assert_eq!(all.len() as u64, 5 * BLOCK);
        let mut rng = Rng::new(9);
        for _ in 0..2000 {
            let at = rng.below(all.len() as u64);
            let mut buf = vec![0u8; usize::try_from(rng.range(1, 40_000)).unwrap()];
            let n = s.read_at(&mut buf, ByteOffset(at)).unwrap();
            assert!(n > 0);
            let at = usize::try_from(at).unwrap();
            assert_eq!(&buf[..n], &all[at..at + n]);
        }
    }

    #[test]
    fn blocks_end_with_a_line_and_b_differs_a_little() {
        let (a, b) = (
            whole(&Synthetic::new(5, 64 * BLOCK, 0.001, Side::A)),
            whole(&Synthetic::new(5, 64 * BLOCK, 0.001, Side::B)),
        );
        for k in 1..=64 {
            let end = usize::try_from(k * BLOCK).unwrap();
            assert_eq!(a[end - 1], b'\n');
            assert_eq!(b[end - 1], b'\n');
        }
        let lines = |d: &[u8]| -> Vec<Vec<u8>> {
            d.split_inclusive(|&c| c == b'\n')
                .map(<[u8]>::to_vec)
                .collect()
        };
        let (la, lb) = (lines(&a), lines(&b));
        let set: std::collections::HashSet<_> = la.iter().collect();
        let new = lb.iter().filter(|l| !set.contains(l)).count();
        assert!(new > 0, "some lines changed");
        assert!(new * 100 < lb.len(), "{new} of {} lines new", lb.len());
        assert_ne!(a, b);
        let same = Synthetic::new(5, 64 * BLOCK, 0.0, Side::B);
        assert_eq!(whole(&same), a, "no change, no difference");
    }
}
