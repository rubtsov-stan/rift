//! Longest increasing subsequence over anchors and the resulting segment map.
//!
//! The anchor candidates, ordered by line in A, are reduced to the longest
//! chain that also increases in B (patience sorting). Each anchor opens a
//! gap that runs to the next anchor, so every line of both files lies in
//! exactly one gap. A gap whose two sides have the same line count and byte
//! length is compared byte by byte, streaming through two fixed buffers,
//! and runs of such gaps are read as one range, so identical stretches cost
//! one sequential pass. The comparison runs on the rayon pool, one stretch
//! of A per task, since one thread alternating between two files reads at
//! a fraction of the disk's speed. Equal neighbours merge into one segment;
//! any other gap is `Differs`.
//!
//! An anchor is confirmed byte by byte either way: inside the comparison of
//! the equal gap it opens, or, when its gap differs, by reading its two
//! lines. An anchor whose lines differ is demoted: the gaps on both sides
//! of it merge into one `Differs` segment. An `Equal` segment is always a
//! compared one.
//!
//! Memory, from the skeleton half, besides what the two [`Scanned`] hold:
//! the anchor table and candidates (see [`anchors`]), then 8 bytes per
//! candidate during the subsequence search, then two buffers of at most
//! 1 MiB per pool thread and a bit per gap during the comparison, and
//! 40 bytes per segment for the result.

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use rayon::iter::{IntoParallelIterator, ParallelIterator};

use crate::anchors::{self, Anchors, Pair};
use crate::budget::{Budget, Half, Reservation};
use crate::error::{Error, Result};
use crate::sample::Sample;
use crate::scan::{ScanConfig, Scanned, scan};
use crate::segment::{Segment, SegmentStatus};
use crate::source::Source;
use crate::strata::MAX_LEVEL;
use crate::types::{ByteOffset, Bytes, LineNo};

/// Largest comparison buffer, per file.
const MAX_BUF: u64 = 1 << 20;
/// Smallest comparison buffer, per file.
const MIN_BUF: u64 = 4 << 10;
const SEGMENT_BYTES: u64 = size_of::<Segment>() as u64;
/// Marks the first element of a chain in the subsequence search.
const NO_PREV: u32 = u32::MAX;

/// The segment map of a pair of files. Dropping it releases its share of
/// the budget.
#[derive(Debug)]
pub struct Skeleton {
    segments: Vec<Segment>,
    anchors: usize,
    level: u32,
    _reservation: Reservation,
}

impl Skeleton {
    /// Segments in file order, tiling both files.
    #[must_use]
    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// Anchors in the chain the map was cut along, all confirmed byte by
    /// byte. A candidate whose bytes turned out to differ (a hash
    /// collision) is not counted, and its neighbouring gaps form one
    /// `Differs` segment.
    #[must_use]
    pub fn anchors(&self) -> usize {
        self.anchors
    }

    /// The stratum the anchors were drawn from, after any thinning.
    #[must_use]
    pub fn level(&self) -> u32 {
        self.level
    }

    /// Whether the files are identical: every segment is `Equal`.
    #[must_use]
    pub fn is_identical(&self) -> bool {
        self.segments
            .iter()
            .all(|s| s.status == SegmentStatus::Equal)
    }
}

/// Scans both sources with [`ScanConfig::for_pair`] and builds their map.
///
/// # Errors
///
/// As for [`scan`] and [`build`].
pub fn build_pair<A: Source + ?Sized, B: Source + ?Sized>(
    src_a: &A,
    src_b: &B,
    budget: &Budget,
) -> Result<(Scanned, Scanned, Skeleton)> {
    let (cfg_a, cfg_b) = ScanConfig::for_pair(src_a.len(), src_b.len(), budget);
    let mut scan_a = scan(src_a, budget, &cfg_a)?;
    let mut scan_b = scan(src_b, budget, &cfg_b)?;
    let skeleton = build(src_a, src_b, &mut scan_a, &mut scan_b, budget)?;
    Ok((scan_a, scan_b, skeleton))
}

/// Builds the segment map of `src_a` and `src_b` from their scans.
///
/// Both scans are thinned to a common stratum first, and further whenever
/// the anchor table does not fit what is left of the skeleton half.
/// `O(n_a + n_b + m log m)` for `n` samples and `m` anchor candidates, plus
/// one read of every byte that lies in a gap of equal length on both sides.
///
/// # Errors
///
/// [`Error::Budget`] if the skeleton half cannot hold the table even at
/// the coarsest stratum, the comparison buffers, or the segment list;
/// [`Error::Io`] from either source.
pub fn build<A: Source + ?Sized, B: Source + ?Sized>(
    src_a: &A,
    src_b: &B,
    scan_a: &mut Scanned,
    scan_b: &mut Scanned,
    budget: &Budget,
) -> Result<Skeleton> {
    let mut level = scan_a.level().max(scan_b.level());
    let mut anchors = loop {
        scan_a.thin(level);
        scan_b.thin(level);
        match anchors::select(scan_a.samples(), scan_b.samples(), budget) {
            Err(Error::Budget(_)) if level < MAX_LEVEL => level += 1,
            other => break other?,
        }
    };
    longest_increasing(&mut anchors, budget)?;

    let bounds = Bounds {
        chain: anchors.pairs(),
        samples_a: scan_a.samples(),
        samples_b: scan_b.samples(),
        end_a: Pos {
            line: scan_a.index().line_count(),
            offset: src_a.len().0,
        },
        end_b: Pos {
            line: scan_b.index().line_count(),
            offset: src_b.len().0,
        },
    };
    let gaps = bounds.chain.len() + 1;
    let threads = rayon::current_num_threads();
    let buf =
        (budget.available(Half::Skeleton).0 / (8 * 2 * threads as u64)).clamp(MIN_BUF, MAX_BUF);
    let bitmap = 8 * gaps.div_ceil(64) as u64;
    let buffers = 2 * buf * threads as u64;
    let mut work = budget.try_reserve(Half::Skeleton, Bytes(buffers + 2 * bitmap))?;
    let verdicts = compare_gaps(&bounds, src_a, src_b, to_usize(buf), TASK_BYTES)?;
    work.shrink(Bytes(buffers));

    // A false anchor is demoted: the gap it closes is marked as differing
    // and joins the one it opens.
    let differs = |g: usize| verdicts.differs(g) || (g + 1 < gaps && verdicts.false_anchor(g + 1));
    // Equal gaps merge, so segments alternate at worst: at most two per
    // differing gap plus one, which sizes the list once.
    let differing = (0..gaps).filter(|&g| differs(g)).count();
    let cap = gaps.min(2 * differing + 1);
    let reservation = budget.try_reserve(Half::Skeleton, Bytes(cap as u64 * SEGMENT_BYTES))?;
    let mut segments = Vec::with_capacity(cap);
    for g in 0..gaps {
        let status = if differs(g) {
            SegmentStatus::Differs
        } else {
            SegmentStatus::Equal
        };
        let (a, b) = bounds.lines(g, g + 1);
        push(&mut segments, a, b, status, verdicts.false_anchor(g));
    }
    debug_assert!(segments.len() <= cap);
    let false_anchors = (0..gaps).filter(|&g| verdicts.false_anchor(g)).count();
    drop(verdicts);
    drop(work);
    Ok(Skeleton {
        anchors: bounds.chain.len() - false_anchors,
        level,
        segments,
        _reservation: reservation,
    })
}

/// A's bytes per comparison task. Large enough that a task streams for a
/// while, small enough to spread a pair over every core.
// A gap is never split across tasks, so a pair with few anchors
// (long runs of repeated lines) compares on one thread; split long gaps by
// byte range if that case matters.
const TASK_BYTES: u64 = 16 << 20;

/// What the comparison found, per gap.
struct Verdicts {
    /// A bit per gap, set when it differs or is not aligned.
    differs: Vec<AtomicU64>,
    /// A bit per gap, set when the anchor opening it turned out to have
    /// unequal lines: a hash collision.
    false_anchors: Vec<AtomicU64>,
}

fn bitmap(bits: usize) -> Vec<AtomicU64> {
    (0..bits.div_ceil(64)).map(|_| AtomicU64::new(0)).collect()
}

// Relaxed: each bit is written by one task and read after the pool joins.
fn set(bits: &[AtomicU64], i: usize) {
    bits[i / 64].fetch_or(1 << (i % 64), Ordering::Relaxed);
}

fn get(bits: &[AtomicU64], i: usize) -> bool {
    bits[i / 64].load(Ordering::Relaxed) & (1 << (i % 64)) != 0
}

impl Verdicts {
    fn differs(&self, g: usize) -> bool {
        get(&self.differs, g)
    }

    fn false_anchor(&self, g: usize) -> bool {
        get(&self.false_anchors, g)
    }
}

/// Compares every aligned gap, and reads the anchor line of every gap that
/// differs, which is how those anchors get confirmed.
///
/// Tasks own the gaps that start in consecutive `task_bytes` of A and run
/// on the rayon pool, each with two `buf`-byte buffers; at most one task
/// runs per thread, so the buffers stay within what the caller reserved.
/// Inside a task, consecutive aligned gaps stream as one range, so a stretch
/// of equal gaps costs sequential reads of the buffer size, and an anchor
/// line is read right after the bytes before it, while still cached.
fn compare_gaps<A: Source + ?Sized, B: Source + ?Sized>(
    bounds: &Bounds<'_>,
    src_a: &A,
    src_b: &B,
    buf: usize,
    task_bytes: u64,
) -> Result<Verdicts> {
    let gaps = bounds.chain.len() + 1;
    let verdicts = Verdicts {
        differs: bitmap(gaps),
        false_anchors: bitmap(gaps),
    };
    let differs = |g: usize, buf_a: &mut [u8], buf_b: &mut [u8]| -> Result<()> {
        set(&verdicts.differs, g);
        if g > 0 {
            let (a, b) = bounds.at(g);
            let (a, b) = (ByteOffset(a.offset), ByteOffset(b.offset));
            if !anchors::same_line(src_a, a, src_b, b, buf_a, buf_b)? {
                set(&verdicts.false_anchors, g);
            }
        }
        Ok(())
    };
    let tasks = bounds.end_a.offset.div_ceil(task_bytes).max(1);
    let first_gap = |task: u64| match task {
        0 => 0,
        t if t >= tasks => gaps,
        t => bounds.first_gap_from(t * task_bytes),
    };
    (0..tasks).into_par_iter().try_for_each_init(
        || (vec![0u8; buf], vec![0u8; buf]),
        |(buf_a, buf_b), task| -> Result<()> {
            let (lo, hi) = (first_gap(task), first_gap(task + 1));
            let mut i = lo;
            while i < hi {
                if !bounds.aligned(i) {
                    differs(i, buf_a, buf_b)?;
                    i += 1;
                    continue;
                }
                let mut j = i + 1;
                while j < hi && bounds.aligned(j) {
                    j += 1;
                }
                // Gaps i..j have the same shape on both sides, so one stream
                // covers them and a mismatch position names its gap.
                while i < j {
                    let (start_a, start_b) = bounds.at(i);
                    let len = bounds.at(j).0.offset - start_a.offset;
                    let Some(at) = first_mismatch(
                        src_a,
                        src_b,
                        start_a.offset,
                        start_b.offset,
                        len,
                        buf_a,
                        buf_b,
                    )?
                    else {
                        break;
                    };
                    let bad = bounds.gap_at(i, j, start_a.offset + at);
                    differs(bad, buf_a, buf_b)?;
                    i = bad + 1;
                }
                i = j;
            }
            Ok(())
        },
    )?;
    Ok(verdicts)
}

/// Reduces the candidates, ordered by A, to a longest chain increasing in
/// B. Patience sorting: `O(m log m)` time, 8 bytes per candidate.
fn longest_increasing(anchors: &mut Anchors, budget: &Budget) -> Result<()> {
    let pairs = anchors.pairs_mut();
    let m = pairs.len();
    if m == 0 {
        return Ok(());
    }
    let work = budget.try_reserve(Half::Skeleton, Bytes(8 * m as u64))?;
    // tails[k]: candidate ending the best chain of length k + 1 found so far.
    let mut tails: Vec<u32> = Vec::with_capacity(m);
    let mut prev: Vec<u32> = Vec::with_capacity(m);
    for i in 0..m {
        let b = pairs[i].b;
        let k = tails.partition_point(|&t| pairs[t as usize].b < b);
        prev.push(if k == 0 { NO_PREV } else { tails[k - 1] });
        let i = anchors::index(i);
        if k == tails.len() {
            tails.push(i);
        } else {
            tails[k] = i;
        }
    }
    // Walk the chain back into `tails`, then gather it to the front of
    // `pairs`: chain[j] sits at or after position j, so nothing is
    // overwritten before it is read.
    let len = tails.len();
    let mut cur = tails[len - 1];
    for slot in tails.iter_mut().rev() {
        *slot = cur;
        cur = prev[cur as usize];
    }
    for (j, &t) in tails.iter().enumerate() {
        pairs[j] = pairs[t as usize];
    }
    pairs.truncate(len);
    drop(tails);
    drop(prev);
    anchors.release_slack();
    drop(work);
    Ok(())
}

fn to_usize(n: u64) -> usize {
    usize::try_from(n).expect("buffer size fits usize")
}

#[derive(Debug, Clone, Copy)]
struct Pos {
    line: u64,
    offset: u64,
}

impl Pos {
    fn of(s: &Sample) -> Self {
        Self {
            line: s.line.0,
            offset: s.offset.0,
        }
    }
}

/// Gap boundaries: boundary 0 is the start of both files, boundary `i`
/// the `i`-th anchor of the chain, and the last one the end of both files.
/// Gap `g` runs from boundary `g` to boundary `g + 1`.
struct Bounds<'s> {
    chain: &'s [Pair],
    samples_a: &'s [Sample],
    samples_b: &'s [Sample],
    end_a: Pos,
    end_b: Pos,
}

impl Bounds<'_> {
    fn at(&self, i: usize) -> (Pos, Pos) {
        if i == 0 {
            let zero = Pos { line: 0, offset: 0 };
            (zero, zero)
        } else if let Some(p) = self.chain.get(i - 1) {
            (
                Pos::of(&self.samples_a[p.a as usize]),
                Pos::of(&self.samples_b[p.b as usize]),
            )
        } else {
            (self.end_a, self.end_b)
        }
    }

    /// Whether gap `g` has the same line count and byte length on both
    /// sides, the precondition for comparing it.
    fn aligned(&self, g: usize) -> bool {
        let ((a0, b0), (a1, b1)) = (self.at(g), self.at(g + 1));
        a1.line - a0.line == b1.line - b0.line && a1.offset - a0.offset == b1.offset - b0.offset
    }

    /// The first gap that starts at or after byte `offset` of A.
    fn first_gap_from(&self, offset: u64) -> usize {
        // Gap g > 0 starts at anchor g - 1, in increasing A order.
        1 + self
            .chain
            .partition_point(|p| self.samples_a[p.a as usize].offset.0 < offset)
    }

    /// The gap among `lo..hi` whose A side holds byte `offset`.
    fn gap_at(&self, mut lo: usize, mut hi: usize, offset: u64) -> usize {
        while lo + 1 < hi {
            let mid = lo + (hi - lo) / 2;
            if self.at(mid).0.offset <= offset {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// Line ranges from boundary `from` to boundary `to`.
    fn lines(&self, from: usize, to: usize) -> (Range<LineNo>, Range<LineNo>) {
        let ((a0, b0), (a1, b1)) = (self.at(from), self.at(to));
        (
            LineNo(a0.line)..LineNo(a1.line),
            LineNo(b0.line)..LineNo(b1.line),
        )
    }
}

/// Appends a segment, merging it into an `Equal` predecessor when it is
/// `Equal` too, or into a `Differs` one when `join` says the boundary
/// between them is a false anchor. A segment empty on both sides is dropped.
fn push(
    segments: &mut Vec<Segment>,
    a: Range<LineNo>,
    b: Range<LineNo>,
    status: SegmentStatus,
    join: bool,
) {
    if a.is_empty() && b.is_empty() {
        return;
    }
    if let Some(last) = segments.last_mut()
        && last.status == status
        && (status == SegmentStatus::Equal || join)
    {
        last.a.end = a.end;
        last.b.end = b.end;
        return;
    }
    segments.push(Segment { a, b, status });
}

/// Position of the first byte that differs between `len` bytes at
/// `start_a` in A and at `start_b` in B, or `None` if all match. Streams
/// through the two buffers.
fn first_mismatch<A: Source + ?Sized, B: Source + ?Sized>(
    src_a: &A,
    src_b: &B,
    start_a: u64,
    start_b: u64,
    len: u64,
    buf_a: &mut [u8],
    buf_b: &mut [u8],
) -> Result<Option<u64>> {
    let step = buf_a.len().min(buf_b.len()) as u64;
    let mut pos = 0;
    while pos < len {
        let n = to_usize((len - pos).min(step));
        let (a, b) = (&mut buf_a[..n], &mut buf_b[..n]);
        src_a.read_exact_at(a, ByteOffset(start_a + pos))?;
        src_b.read_exact_at(b, ByteOffset(start_b + pos))?;
        if a != b {
            let i = a.iter().zip(b.iter()).position(|(x, y)| x != y);
            return Ok(Some(pos + i.expect("slices differ") as u64));
        }
        pos += n as u64;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::{LineAccess, MemLines};

    fn samples(data: &[u8]) -> Vec<Sample> {
        let mut offset = 0;
        data.split_inclusive(|&c| c == b'\n')
            .enumerate()
            .map(|(i, l)| {
                let s = Sample {
                    hash: 0,
                    line: LineNo(i as u64),
                    offset: ByteOffset(offset),
                };
                offset += l.len() as u64;
                s
            })
            .collect()
    }

    #[test]
    fn the_chain_is_a_longest_increasing_one() {
        let with = |hashes: &[u64]| -> Vec<Sample> {
            let mut v = samples(&b"x\n".repeat(hashes.len()));
            for (s, &h) in v.iter_mut().zip(hashes) {
                s.hash = h;
            }
            v
        };
        let budget = Budget::new(Bytes::mib(1));
        let (a, b) = (with(&[1, 2, 3, 4, 5, 6]), with(&[3, 1, 2, 6, 4, 5]));
        let mut anchors = anchors::select(&a, &b, &budget).unwrap();
        longest_increasing(&mut anchors, &budget).unwrap();
        let chain: Vec<(u32, u32)> = anchors.pairs().iter().map(|p| (p.a, p.b)).collect();
        assert_eq!(chain, [(0, 1), (1, 2), (3, 4), (4, 5)]);
    }

    /// Splitting the comparison into tasks of any size gives the same
    /// verdicts as comparing every gap on its own.
    #[test]
    fn task_boundaries_do_not_change_the_verdicts() {
        let a: Vec<u8> = (0..300)
            .flat_map(|i| format!("line {i}\n").into_bytes())
            .collect();
        let mut b = a.clone();
        for at in [5, 77, 78, 1500, a.len() - 2] {
            b[at] = b'#';
        }
        let (sa, sb) = (samples(&a), samples(&b));
        let chain: Vec<Pair> = (0..300).step_by(7).map(|i| Pair { a: i, b: i }).collect();
        let (ma, mb) = (MemLines::new(&a), MemLines::new(&b));
        let bounds = Bounds {
            chain: &chain,
            samples_a: &sa,
            samples_b: &sb,
            end_a: Pos {
                line: ma.line_count(),
                offset: a.len() as u64,
            },
            end_b: Pos {
                line: mb.line_count(),
                offset: b.len() as u64,
            },
        };
        let gaps = chain.len() + 1;
        let want: Vec<bool> = (0..gaps)
            .map(|g| {
                let ((a0, b0), (a1, b1)) = (bounds.at(g), bounds.at(g + 1));
                let r = |x: Pos, y: Pos| to_usize(x.offset)..to_usize(y.offset);
                a[r(a0, a1)] != b[r(b0, b1)]
            })
            .collect();
        assert!(want.iter().filter(|&&d| d).count() >= 4);
        // Anchor g - 1 opens gap g; its line differs when a change hit it.
        let false_anchors = (1..gaps)
            .filter(|&g| {
                want[g] && ma.line(u64::from(chain[g - 1].a)) != mb.line(u64::from(chain[g - 1].b))
            })
            .collect::<Vec<_>>();
        assert!(!false_anchors.is_empty());
        for task_bytes in [1, 3, 64, 100, 1 << 20] {
            for buf in [1, 5, 4096] {
                let got = compare_gaps(&bounds, &a[..], &b[..], buf, task_bytes).unwrap();
                let bits: Vec<bool> = (0..gaps).map(|g| got.differs(g)).collect();
                assert_eq!(bits, want, "task_bytes {task_bytes}, buf {buf}");
                let false_got: Vec<usize> = (0..gaps).filter(|&g| got.false_anchor(g)).collect();
                assert_eq!(false_got, false_anchors, "task_bytes {task_bytes}");
            }
        }
    }
}
