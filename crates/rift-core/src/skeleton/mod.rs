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
//! 56 bytes per segment for the result: the segment and the byte offsets
//! where it starts. The candidates left out of the chain stay as an
//! [`OffChain`] list, 16 bytes per pair, capped as described there.

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use memchr::memchr;
use rayon::iter::{IntoParallelIterator, ParallelIterator};

use crate::anchors::{self, Anchors, Pair};
use crate::budget::{Budget, BudgetExceeded, Half, Reservation};
use crate::error::{Error, Result};
use crate::sample::Sample;
use crate::scan::{Meter, ScanConfig, Scanned, Watch, release_slack, scan_metered};
use crate::segment::{Segment, SegmentStatus};
use crate::source::Source;
use crate::strata::MAX_LEVEL;
use crate::types::{ByteOffset, Bytes, LineNo};

/// Largest comparison buffer, per file.
const MAX_BUF: u64 = 1 << 20;
/// Smallest comparison buffer, per file.
const MIN_BUF: u64 = 4 << 10;
/// A segment and the byte offsets where it starts in both files.
const SEGMENT_BYTES: u64 = (size_of::<Segment>() + size_of::<[ByteOffset; 2]>()) as u64;
const SAMPLE_BYTES: u64 = size_of::<Sample>() as u64;
/// The skeleton half a map needs at the coarsest stratum, with no anchors:
/// a one-slot table, one segment, two bitmap words and one lane of the
/// smallest comparison buffers.
const COARSEST_MAP_BYTES: u64 = 16 + SEGMENT_BYTES + 16 + 2 * MIN_BUF;
/// Marks the first element of a chain in the subsequence search.
const NO_PREV: u32 = u32::MAX;
/// One off-chain pair: its line in A and in B.
const LINE_PAIR_BYTES: u64 = size_of::<[LineNo; 2]>() as u64;
/// The off-chain list takes at most this fraction, `1 / OFF_CHAIN_SHARE`,
/// of the skeleton half that is free when the chain search starts.
const OFF_CHAIN_SHARE: u64 = 8;
/// The off-chain list grows by at least this many pairs at a time.
const OFF_CHAIN_STEP: usize = 64;

/// Anchor candidates that the chain search left out: hash matches between
/// a line of A and a line of B that cross the chain, such as the lines of
/// a block that moved.
///
/// These are hash matches, not confirmed ones: unlike the chain, nothing
/// reads their bytes, so a reader compares the two lines before it relies
/// on a pair.
///
/// Memory, from the skeleton half: 16 bytes per pair, reserved as the list
/// grows and trimmed to its length once the chain is known. The list keeps
/// the first pairs in A order up to `1 / 8` of the skeleton half that is
/// free when the chain search starts, so it never crowds out the segment
/// list and comparison buffers that follow. The rest are counted in
/// [`dropped`](Self::dropped). Dropping the list releases its bytes.
#[derive(Debug)]
pub struct OffChain {
    pairs: Vec<[LineNo; 2]>,
    dropped: u64,
    limit: usize,
    reservation: Reservation,
}

impl OffChain {
    fn new(budget: &Budget) -> Result<Self> {
        let limit = budget.available(Half::Skeleton).0 / OFF_CHAIN_SHARE / LINE_PAIR_BYTES;
        Ok(Self {
            pairs: Vec::new(),
            dropped: 0,
            limit: usize::try_from(limit).unwrap_or(usize::MAX),
            reservation: budget.try_reserve(Half::Skeleton, Bytes(0))?,
        })
    }

    /// Appends a pair if the cap and the budget allow, counts it as
    /// dropped otherwise.
    fn push(&mut self, pair: [LineNo; 2]) {
        let len = self.pairs.len();
        if len == self.pairs.capacity() {
            let extra = (2 * len)
                .max(OFF_CHAIN_STEP)
                .min(self.limit)
                .saturating_sub(len);
            let bytes = Bytes(extra as u64 * LINE_PAIR_BYTES);
            if extra == 0 || self.reservation.grow(bytes).is_err() {
                self.limit = len;
                self.dropped += 1;
                return;
            }
            if self.pairs.try_reserve_exact(extra).is_err() {
                self.reservation.shrink(bytes);
                self.limit = len;
                self.dropped += 1;
                return;
            }
        }
        self.pairs.push(pair);
    }

    /// The kept pairs, `[line in A, line in B]`, ordered by the line in A.
    #[must_use]
    pub fn pairs(&self) -> &[[LineNo; 2]] {
        &self.pairs
    }

    /// Pairs left out because the list reached its cap.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped
    }
}

/// The segment map of a pair of files. Dropping it releases its share of
/// the budget.
#[derive(Debug)]
pub struct Skeleton {
    segments: Vec<Segment>,
    starts: Vec<[ByteOffset; 2]>,
    anchors: usize,
    level: u32,
    reservation: Reservation,
    off_chain: OffChain,
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

    /// Byte offsets where each segment starts in A and in B, known from
    /// the anchors, so a reader can go there without scanning.
    #[must_use]
    pub fn starts(&self) -> &[[ByteOffset; 2]] {
        &self.starts
    }

    /// Candidates the chain left out; see [`OffChain`].
    #[must_use]
    pub fn off_chain(&self) -> &OffChain {
        &self.off_chain
    }

    /// The segment list, the start offsets, the skeleton-half bytes they
    /// hold, and the off-chain list, which holds its own.
    #[must_use]
    pub fn into_parts(self) -> (Vec<Segment>, Vec<[ByteOffset; 2]>, Reservation, OffChain) {
        (self.segments, self.starts, self.reservation, self.off_chain)
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
/// As for [`scan`](crate::scan::scan) and [`build`].
pub fn build_pair<A: Source + ?Sized, B: Source + ?Sized>(
    src_a: &A,
    src_b: &B,
    budget: &Budget,
) -> Result<(Scanned, Scanned, Skeleton)> {
    build_pair_watched(src_a, src_b, budget, &Watch::new())
}

/// [`build_pair`] followed and stopped through `watch`.
///
/// The first pass counts in `watch` every chunk of A, then of B, as it is
/// scanned; the second counts both sides of every stretch it compares and
/// of every gap it does not need to read. So the count reaches twice the
/// size of both files when the map is built.
///
/// # Errors
///
/// As for [`build_pair`], and [`Error::Cancelled`] once `watch` is
/// cancelled.
pub fn build_pair_watched<A: Source + ?Sized, B: Source + ?Sized>(
    src_a: &A,
    src_b: &B,
    budget: &Budget,
    watch: &Watch,
) -> Result<(Scanned, Scanned, Skeleton)> {
    let (len_a, len_b) = (src_a.len().0, src_b.len().0);
    let (cfg_a, cfg_b) = ScanConfig::for_pair(src_a.len(), src_b.len(), budget);
    let mut scan_a = scan_metered(src_a, budget, &cfg_a, &Meter::new(watch, 0))?;
    let mut scan_b = scan_metered(src_b, budget, &cfg_b, &Meter::new(watch, len_a))?;
    let skeleton = build_watched(
        src_a,
        src_b,
        (&mut scan_a, &mut scan_b),
        budget,
        watch,
        len_a + len_b,
    )?;
    Ok((scan_a, scan_b, skeleton))
}

/// Builds the segment map of `src_a` and `src_b` from their scans.
///
/// Both scans are thinned to a common stratum first, and one stratum
/// further whenever the anchor table, the chain search, the comparison
/// buffers or the segment list do not fit what is left of the skeleton
/// half. A retry repeats the comparison.
/// `O(n_a + n_b + m log m)` for `n` samples and `m` anchor candidates, plus
/// one read of every byte that lies in a gap of equal length on both sides.
///
/// # Errors
///
/// [`Error::Budget`] if the skeleton half cannot hold the map even at the
/// coarsest stratum, at once and without reading when releasing every
/// sample would not be enough; [`Error::Io`] from either source.
pub fn build<A: Source + ?Sized, B: Source + ?Sized>(
    src_a: &A,
    src_b: &B,
    scan_a: &mut Scanned,
    scan_b: &mut Scanned,
    budget: &Budget,
) -> Result<Skeleton> {
    build_watched(src_a, src_b, (scan_a, scan_b), budget, &Watch::new(), 0)
}

/// [`build`] whose comparison counts in `watch` from `base` bytes.
fn build_watched<A: Source + ?Sized, B: Source + ?Sized>(
    src_a: &A,
    src_b: &B,
    (scan_a, scan_b): (&mut Scanned, &mut Scanned),
    budget: &Budget,
    watch: &Watch,
    base: u64,
) -> Result<Skeleton> {
    // Thinning can release the samples and nothing else. If even that
    // leaves too little for the map at the coarsest stratum, say so before
    // reading anything.
    let releasable = (scan_a.samples().len() + scan_b.samples().len()) as u64 * SAMPLE_BYTES;
    let available = budget.available(Half::Skeleton).0;
    if available + releasable < COARSEST_MAP_BYTES {
        return Err(BudgetExceeded {
            half: Half::Skeleton,
            requested: Bytes(COARSEST_MAP_BYTES),
            available: Bytes(available + releasable),
        }
        .into());
    }
    let mut level = scan_a.level().max(scan_b.level());
    loop {
        scan_a.thin(level);
        scan_b.thin(level);
        let meter = Meter::new(watch, base);
        meter.check()?;
        match build_at(src_a, src_b, scan_a, scan_b, budget, &meter) {
            Err(Error::Budget(_)) if level < MAX_LEVEL => level += 1,
            other => return other,
        }
    }
}

/// One attempt at the scans' current stratum.
fn build_at<A: Source + ?Sized, B: Source + ?Sized>(
    src_a: &A,
    src_b: &B,
    scan_a: &Scanned,
    scan_b: &Scanned,
    budget: &Budget,
    meter: &Meter<'_>,
) -> Result<Skeleton> {
    let mut anchors = anchors::select(scan_a.samples(), scan_b.samples(), budget)?;
    let mut off_chain = OffChain::new(budget)?;
    longest_increasing(&mut anchors, budget, |p| {
        off_chain.push([
            scan_a.samples()[p.a as usize].line,
            scan_b.samples()[p.b as usize].line,
        ]);
    })?;
    release_slack(&mut off_chain.pairs, &mut off_chain.reservation);

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
    // The segment list is reserved for its worst case before anything is
    // read, so a shortfall shows up, and the stratum is raised, before the
    // comparison rather than after it.
    let mut reservation = budget.try_reserve(Half::Skeleton, Bytes(gaps as u64 * SEGMENT_BYTES))?;
    let bitmaps = 2 * 8 * gaps.div_ceil(64) as u64;
    // As many lanes as there are tasks, `MAX_THREADS` and pairs of the
    // smallest buffers that fit, whatever the pool runs them on. The buffers take half of what is free, since the
    // segment list is already reserved and nothing else needs the rest
    // during the comparison, but no more than the longest stretch a task
    // streams: a small pair under a large budget gets small buffers.
    let free = budget.available(Half::Skeleton).0.saturating_sub(bitmaps);
    let tasks = bounds.end_a.offset.div_ceil(TASK_BYTES).max(1);
    let lanes = (free / (2 * MIN_BUF))
        .min(tasks)
        .clamp(1, crate::budget::MAX_THREADS as u64);
    let buf = (free / (2 * 2 * lanes))
        .min(bounds.longest_aligned_run().max(MIN_BUF))
        .clamp(MIN_BUF, MAX_BUF);
    let buffers = 2 * buf * lanes;
    let mut work = budget.try_reserve(Half::Skeleton, Bytes(buffers + bitmaps))?;
    let verdicts = compare_gaps(
        &bounds,
        src_a,
        src_b,
        Bytes(buf).as_usize(),
        usize::try_from(lanes).expect("lanes are at most the thread count"),
        TASK_BYTES,
        meter,
    )?;
    work.shrink(Bytes(buffers));

    // A false anchor is demoted: the gap it closes is marked as differing
    // and joins the one it opens.
    let differs = |g: usize| verdicts.differs(g) || (g + 1 < gaps && verdicts.false_anchor(g + 1));
    // Equal gaps merge, so segments alternate at worst: at most two per
    // differing gap plus one, which sizes the list once.
    let differing = (0..gaps).filter(|&g| differs(g)).count();
    let cap = gaps.min(2 * differing + 1);
    reservation.shrink(Bytes((gaps - cap) as u64 * SEGMENT_BYTES));
    let mut segments = Vec::with_capacity(cap);
    let mut starts = Vec::with_capacity(cap);
    for g in 0..gaps {
        let status = if differs(g) {
            SegmentStatus::Differs
        } else {
            SegmentStatus::Equal
        };
        let (a, b) = bounds.lines(g, g + 1);
        if push(&mut segments, a, b, status, verdicts.false_anchor(g)) {
            let (pa, pb) = bounds.at(g);
            starts.push([ByteOffset(pa.offset), ByteOffset(pb.offset)]);
        }
    }
    debug_assert!(segments.len() <= cap);
    let false_anchors = (0..gaps).filter(|&g| verdicts.false_anchor(g)).count();
    drop(verdicts);
    drop(work);
    Ok(Skeleton {
        anchors: bounds.chain.len() - false_anchors,
        level: scan_a.level(),
        segments,
        starts,
        reservation,
        off_chain,
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
/// Tasks own the gaps that start in consecutive `task_bytes` of A. They are
/// dealt round-robin to `lanes` jobs on the rayon pool, each job with two
/// `buf`-byte buffers, so the buffers stay within what the caller reserved.
/// Inside a task, consecutive aligned gaps stream as one range, so a stretch
/// of equal gaps costs sequential reads of the buffer size. The anchor
/// lines of differing gaps are read where they come, cold, spread over
/// the lanes.
///
/// Each task counts in `meter` both sides of every stretch it streams, as
/// it reads them, and the rest of its gaps, both sides, when it ends; so
/// the tasks together count both files once.
fn compare_gaps<A: Source + ?Sized, B: Source + ?Sized>(
    bounds: &Bounds<'_>,
    src_a: &A,
    src_b: &B,
    buf: usize,
    lanes: usize,
    task_bytes: u64,
    meter: &Meter<'_>,
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
            if !same_line(src_a, a, src_b, b, buf_a, buf_b, meter)? {
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
    let run = |task: u64, buf_a: &mut [u8], buf_b: &mut [u8]| -> Result<()> {
        meter.check()?;
        let (lo, hi) = (first_gap(task), first_gap(task + 1));
        let (start, end) = (bounds.at(lo), bounds.at(hi));
        let span = (end.0.offset - start.0.offset) + (end.1.offset - start.1.offset);
        let mut counted = 0;
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
            // covers them and a mismatch position names its gap. A stream
            // that restarts after a mismatch starts with a small read.
            let mut first = u64::MAX;
            while i < j {
                let (start_a, start_b) = bounds.at(i);
                let len = bounds.at(j).0.offset - start_a.offset;
                let Some(at) = first_mismatch(
                    src_a,
                    src_b,
                    (start_a.offset, start_b.offset),
                    len,
                    (&mut *buf_a, &mut *buf_b),
                    first,
                    (meter, &mut counted),
                )?
                else {
                    break;
                };
                let bad = bounds.gap_at(i, j, start_a.offset + at);
                differs(bad, buf_a, buf_b)?;
                i = bad + 1;
                first = FIRST_READ as u64;
            }
            i = j;
        }
        meter.add(span.saturating_sub(counted));
        Ok(())
    };
    (0..lanes).into_par_iter().try_for_each(|lane| {
        let (mut buf_a, mut buf_b) = (vec![0u8; buf], vec![0u8; buf]);
        (lane as u64..tasks)
            .step_by(lanes)
            .try_for_each(|task| run(task, &mut buf_a, &mut buf_b))
    })?;
    Ok(verdicts)
}

/// Reduces the candidates, ordered by A, to a longest chain increasing in
/// B, and hands every candidate left out to `off_chain`, in A order.
/// Patience sorting: `O(m log m)` time, 12 bytes per candidate.
pub(crate) fn longest_increasing(
    anchors: &mut Anchors,
    budget: &Budget,
    mut off_chain: impl FnMut(Pair),
) -> Result<()> {
    let pairs = &mut anchors.pairs;
    let m = pairs.len();
    if m == 0 {
        return Ok(());
    }
    let work = budget.try_reserve(Half::Skeleton, Bytes(12 * m as u64))?;
    // tails[k]: candidate ending the best chain of length k + 1 found so
    // far, and ends[k] its B sample, so the search reads one dense array.
    let mut tails: Vec<u32> = Vec::with_capacity(m);
    let mut ends: Vec<u32> = Vec::with_capacity(m);
    let mut prev: Vec<u32> = Vec::with_capacity(m);
    for (i, p) in pairs.iter().enumerate() {
        let b = p.b;
        // Most candidates extend the longest chain, as in files that
        // mostly agree; they skip the search.
        let k = if ends.last().is_none_or(|&e| e < b) {
            ends.len()
        } else {
            ends.partition_point(|&e| e < b)
        };
        prev.push(if k == 0 { NO_PREV } else { tails[k - 1] });
        let i = anchors::index(i);
        if k == tails.len() {
            tails.push(i);
            ends.push(b);
        } else {
            tails[k] = i;
            ends[k] = b;
        }
    }
    drop(ends);
    // Walk the chain back into `tails`, then gather it to the front of
    // `pairs`: chain[j] sits at or after position j, so nothing is
    // overwritten before it is read.
    let len = tails.len();
    let mut cur = tails[len - 1];
    for slot in tails.iter_mut().rev() {
        *slot = cur;
        cur = prev[cur as usize];
    }
    let mut chain = tails.iter().peekable();
    for (i, &p) in pairs.iter().enumerate() {
        if chain.next_if(|&&t| t as usize == i).is_none() {
            off_chain(p);
        }
    }
    for (j, &t) in tails.iter().enumerate() {
        pairs[j] = pairs[t as usize];
    }
    pairs.truncate(len);
    drop(tails);
    drop(prev);
    release_slack(&mut anchors.pairs, &mut anchors.reservation);
    drop(work);
    Ok(())
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

    /// Bytes of A in the longest run of consecutive aligned gaps.
    fn longest_aligned_run(&self) -> u64 {
        let gaps = self.chain.len() + 1;
        let (mut longest, mut start) = (0, None);
        for g in 0..=gaps {
            let aligned = g < gaps && self.aligned(g);
            match (aligned, start) {
                (true, None) => start = Some(self.at(g).0.offset),
                (false, Some(from)) => {
                    longest = longest.max(self.at(g).0.offset - from);
                    start = None;
                }
                _ => {}
            }
        }
        longest
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
/// Returns whether a new segment was added.
fn push(
    segments: &mut Vec<Segment>,
    a: Range<LineNo>,
    b: Range<LineNo>,
    status: SegmentStatus,
    join: bool,
) -> bool {
    if a.is_empty() && b.is_empty() {
        return false;
    }
    if let Some(last) = segments.last_mut()
        && last.status == status
        && (status == SegmentStatus::Equal || join)
    {
        last.a.end = a.end;
        last.b.end = b.end;
        return false;
    }
    segments.push(Segment { a, b, status });
    true
}

/// Bytes read from each file when confirming a line, before the rest of
/// the buffer is used: enough for a typical dump or CSV line in one read.
const FIRST_READ: usize = 256;

/// Whether the line starting at `off_a` in `a` has the same bytes as the
/// line starting at `off_b` in `b`, terminators included. Reads in steps of
/// the buffer size, so a line of any length fits.
///
/// # Panics
///
/// Panics if a buffer is empty.
fn same_line<A: Source + ?Sized, B: Source + ?Sized>(
    a: &A,
    off_a: ByteOffset,
    b: &B,
    off_b: ByteOffset,
    buf_a: &mut [u8],
    buf_b: &mut [u8],
    meter: &Meter<'_>,
) -> Result<bool> {
    let step = buf_a.len().min(buf_b.len());
    assert!(step > 0, "empty comparison buffer");
    // Never ask past the end, so every read can be exact.
    let left = |src: Bytes, at: ByteOffset, want: usize| {
        Bytes(src.0.saturating_sub(at.0)).as_usize().min(want)
    };
    let mut want = FIRST_READ.min(step);
    let mut pos = 0u64;
    loop {
        let (at_a, at_b) = (off_a + pos, off_b + pos);
        meter.check()?;
        let (na, nb) = (left(a.len(), at_a, want), left(b.len(), at_b, want));
        let (ca, cb) = (&mut buf_a[..na], &mut buf_b[..nb]);
        a.read_exact_at(ca, at_a)?;
        b.read_exact_at(cb, at_b)?;
        if let Some(nl) = memchr(b'\n', ca) {
            return Ok(cb.len() > nl && ca[..=nl] == cb[..=nl]);
        }
        if ca != cb {
            return Ok(false);
        }
        if na < want {
            // Both sources ended on the same unterminated line.
            return Ok(true);
        }
        pos += na as u64;
        want = step;
    }
}

/// Position of the first byte that differs between `len` bytes at
/// `starts.0` in A and at `starts.1` in B, or `None` if all match. Streams
/// through the two buffers. Reads start at `first` bytes and double up to
/// the buffer size: the caller restarts the stream after every mismatch,
/// and when mismatches are close a full first read would copy mostly bytes
/// the next restart reads again. Both sides of every read are counted in
/// `meter` and added to `counted`.
fn first_mismatch<A: Source + ?Sized, B: Source + ?Sized>(
    src_a: &A,
    src_b: &B,
    starts: (u64, u64),
    len: u64,
    (buf_a, buf_b): (&mut [u8], &mut [u8]),
    first: u64,
    (meter, counted): (&Meter<'_>, &mut u64),
) -> Result<Option<u64>> {
    let (start_a, start_b) = starts;
    let max = buf_a.len().min(buf_b.len()) as u64;
    let mut step = first.clamp(1, max);
    let mut pos = 0;
    while pos < len {
        let n = Bytes((len - pos).min(step)).as_usize();
        step = (2 * step).min(max);
        let (a, b) = (&mut buf_a[..n], &mut buf_b[..n]);
        meter.check()?;
        src_a.read_exact_at(a, ByteOffset(start_a + pos))?;
        src_b.read_exact_at(b, ByteOffset(start_b + pos))?;
        // Only the bytes before a mismatch: a restart may read the rest
        // again, and a byte counted twice would push the total past both
        // files.
        let same = a.iter().zip(b.iter()).position(|(x, y)| x != y);
        let checked = same.unwrap_or(n) as u64;
        meter.add(2 * checked);
        *counted += 2 * checked;
        if let Some(i) = same {
            return Ok(Some(pos + i as u64));
        }
        pos += n as u64;
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::{LineAccess, MemLines};

    /// A meter on a watch nobody reads or cancels.
    fn meter() -> Meter<'static> {
        static WATCH: std::sync::LazyLock<Watch> = std::sync::LazyLock::new(Watch::new);
        Meter::new(&WATCH, 0)
    }

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

    /// Whether line `i` of `a` matches line `j` of `b`, checked with
    /// `buf`-byte buffers and with ample ones, which must agree.
    fn same(a: &[u8], i: usize, b: &[u8], j: usize, buf: usize) -> bool {
        let off = |d: &[u8], k: usize| {
            ByteOffset(
                d.split_inclusive(|&c| c == b'\n')
                    .take(k)
                    .map(|l| l.len() as u64)
                    .sum(),
            )
        };
        let (oa, ob) = (off(a, i), off(b, j));
        let meter = meter();
        let small = same_line(a, oa, b, ob, &mut vec![0; buf], &mut vec![0; buf], &meter).unwrap();
        let large = same_line(a, oa, b, ob, &mut [0; 4096], &mut [0; 4096], &meter).unwrap();
        assert_eq!(small, large, "buffer size changed the verdict");
        small
    }

    #[test]
    fn same_line_compares_bytes_not_hashes() {
        assert!(same(b"same\nfoo\n", 0, b"same\nbar\n", 0, 64));
        assert!(!same(b"same\nfoo\n", 1, b"same\nbar\n", 1, 64));
        assert!(same(b"a\nb\n", 1, b"b\n", 0, 64));
    }

    #[test]
    fn lines_longer_than_the_buffer_compare_in_steps() {
        let long_a = [vec![b'a'; 1000], b"\n".to_vec()].concat();
        let mut long_b = long_a.clone();
        assert!(same(&long_a, 0, &long_b, 0, 7));
        long_b[999] = b'b';
        assert!(!same(&long_a, 0, &long_b, 0, 7));
        // Same prefix, but B's line goes on past A's terminator.
        assert!(!same(b"ab\n", 0, b"abc\n", 0, 2));
        assert!(!same(b"abc\n", 0, b"ab\n", 0, 2));
    }

    #[test]
    fn unterminated_last_lines_match_only_each_other() {
        assert!(same(b"end", 0, b"end", 0, 2));
        assert!(!same(b"end", 0, b"end\n", 0, 2));
        assert!(!same(b"end\n", 0, b"end", 0, 2));
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
        let mut off = Vec::new();
        longest_increasing(&mut anchors, &budget, |p| off.push((p.a, p.b))).unwrap();
        assert_eq!(off, [(2, 0), (5, 3)]);
        let chain: Vec<(u32, u32)> = anchors.pairs().iter().map(|p| (p.a, p.b)).collect();
        assert_eq!(chain, [(0, 1), (1, 2), (3, 4), (4, 5)]);
    }

    /// Counts the bytes read through it.
    struct Counted<'a>(&'a [u8], AtomicU64);

    impl Source for Counted<'_> {
        fn len(&self) -> Bytes {
            Source::len(self.0)
        }

        fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> std::io::Result<usize> {
            let n = self.0.read_at(buf, offset)?;
            self.1.fetch_add(n as u64, Ordering::Relaxed);
            Ok(n)
        }
    }

    #[test]
    fn close_mismatches_do_not_each_read_a_full_buffer() {
        // 4 000 one-line gaps of 16 bytes, every tenth one changed: 400
        // mismatches in 64 KB.
        let a: Vec<u8> = (0..4000)
            .flat_map(|i| format!("line {i:010}\n").into_bytes())
            .collect();
        let mut b = a.clone();
        for g in (0..4000).step_by(10) {
            b[g * 16 + 3] = b'#';
        }
        let (sa, sb) = (samples(&a), samples(&b));
        let chain: Vec<Pair> = (1..4000).map(|i| Pair { a: i, b: i }).collect();
        let end = |d: &[u8]| Pos {
            line: 4000,
            offset: d.len() as u64,
        };
        let bounds = Bounds {
            chain: &chain,
            samples_a: &sa,
            samples_b: &sb,
            end_a: end(&a),
            end_b: end(&b),
        };
        let (ca, cb) = (
            Counted(&a, AtomicU64::new(0)),
            Counted(&b, AtomicU64::new(0)),
        );
        let verdicts = compare_gaps(&bounds, &ca, &cb, 1 << 20, 1, 1 << 20, &meter()).unwrap();
        assert_eq!((0..4000).filter(|&g| verdicts.differs(g)).count(), 400);
        let read = ca.1.load(Ordering::Relaxed);
        assert!(
            read < 8 * a.len() as u64,
            "read {read} bytes of A for a {} byte file",
            a.len()
        );
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
                let r = |x: Pos, y: Pos| Bytes(x.offset).as_usize()..Bytes(y.offset).as_usize();
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
        assert_ne!(false_anchors, [0; 0]);
        for task_bytes in [1, 3, 64, 100, 1 << 20] {
            for buf in [1, 5, 4096] {
                let got =
                    compare_gaps(&bounds, &a[..], &b[..], buf, 3, task_bytes, &meter()).unwrap();
                let bits: Vec<bool> = (0..gaps).map(|g| got.differs(g)).collect();
                assert_eq!(bits, want, "task_bytes {task_bytes}, buf {buf}");
                let false_got: Vec<usize> = (0..gaps).filter(|&g| got.false_anchor(g)).collect();
                assert_eq!(false_got, false_anchors, "task_bytes {task_bytes}");
            }
        }
    }
}
