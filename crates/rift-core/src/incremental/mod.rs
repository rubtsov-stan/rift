//! Incremental re-diff (experimental): the diff of A and B', guided by the
//! map of an earlier diff of A and B, with the same output as a fresh
//! [`Diff`] of A and B'.
//!
//! A fresh diff reads both files twice: the scan hashes every line, and
//! the skeleton then compares the equal stretches between its anchors
//! byte by byte. Here the earlier map says in advance which byte ranges of
//! A should equal which ranges of B', so a single pass does both jobs for
//! A:
//!
//! 1. B' is scanned as a fresh diff scans it.
//! 2. Each `Equal` range of the earlier map (the guide) is compared byte
//!    by byte, A against B', in parallel, counting line starts on the way.
//! 3. A is never hashed where a range was confirmed: its lines there are
//!    the lines of B', so its samples are those of B', moved to A's line
//!    numbers and offsets, and its index is built from the line starts
//!    counted in step 2. Everything else in A is scanned directly.
//! 4. The skeleton is built from these two scans, as a fresh diff builds
//!    it, except that a gap lying inside a confirmed range on its diagonal
//!    is not read again: its bytes were compared in step 2, so the verdict
//!    is the same.
//! 5. The rest is the fresh diff's own code (`Diff::from_parts`).
//!
//! Output equals a fresh diff's because every input the skeleton and the
//! refinement see is the same: A's scan has the same samples, stratum and
//! index a scan of A would give (they depend only on A's bytes and the
//! configuration), B''s is a real scan, and both halves of the budget hold
//! the same bytes when the skeleton starts. One condition is checked first:
//! a fresh diff scans A, then B' while A's samples are held; when the
//! skeleton half could refuse either scan, the stratum would depend on that
//! refusal, so the run falls back to a fresh diff ([`Fallback::Budget`]).
//!
//! A wrong guide costs time, not correctness: a range whose bytes differ,
//! or that does not start and end at a line start on both sides, is
//! scanned in A like any range outside the guide. When B' is B with lines
//! appended, every `Equal` range of the earlier map still lies at the same
//! offsets, which is the case this path serves. After an insertion or
//! deletion in B', the ranges behind it no longer line up and A is scanned
//! there; translating them through chunk tables ([`cdc`]) is left for later.
//!
//! No line is reported equal on the strength of a hash: a confirmed range
//! was compared byte by byte, and every gap outside one is compared as in a
//! fresh diff.
//!
//! Memory. Skeleton half: A's samples and index, reserved at the most a
//! scan of A may keep (`kept_bytes`) and trimmed to what they hold before
//! the skeleton is built, exactly as a scan's are; B''s scan as usual.
//! Refine half, which a fresh diff leaves empty until the skeleton is
//! built: the guide (24 bytes per range), the work units and their
//! checkpoint slots, and two comparison buffers per lane, all released
//! before the diff takes that half; only the guide is still held while the
//! skeleton is built, which reads the skeleton half alone.

#![allow(
    clippy::many_single_char_names,
    reason = "a and b are the two files and their offsets, as across the crate"
)]

pub mod cdc;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use memchr::memchr_iter;
use rayon::iter::{IntoParallelIterator, ParallelIterator};
use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use crate::budget::{Budget, Half, MAX_THREADS, Reservation};
use crate::diff::{Config, Diff};
use crate::error::{Error, Result};
use crate::index::{Checkpoint, LineIndex};
use crate::sample::Sample;
use crate::scan::{self, Meter, ScanConfig, Scanned, Watch};
use crate::segment::{Segment, SegmentStatus};
use crate::skeleton::{self, Confirmed, Skeleton};
use crate::source::Source;
use crate::strata;
use crate::types::{ByteOffset, Bytes, LineNo};

/// Bytes of a guide range one comparison task covers.
const UNIT: u64 = 16 << 20;
/// Comparison buffer per file and lane, at most and at least.
const MAX_BUF: u64 = 256 << 10;
const MIN_BUF: u64 = 4 << 10;
const CONFIRMED_BYTES: u64 = size_of::<Confirmed>() as u64;
const SAMPLE_BYTES: u64 = size_of::<Sample>() as u64;
const CHECKPOINT_BYTES: u64 = size_of::<Checkpoint>() as u64;
const UNIT_BYTES: u64 = size_of::<Unit>() as u64;
/// Per unit, besides the unit: its slot range and newline count, its
/// entry in a lane's list (index, slots, two result places) and in the
/// lane's answer, and its mismatch flag.
const UNIT_WORK_BYTES: u64 = 16 + 8 + 40 + 16 + 8;

/// The map of an earlier diff of A and B, as [`Diff`] gives it: the
/// segments, where each starts in both files, and the files' lengths.
#[derive(Debug, Clone, Copy)]
pub struct Previous<'p> {
    /// Segments in file order, tiling both files.
    pub segments: &'p [Segment],
    /// Byte offsets where each segment starts in A and in B.
    pub starts: &'p [[ByteOffset; 2]],
    /// Length of A then.
    pub len_a: Bytes,
    /// Length of B then.
    pub len_b: Bytes,
}

/// How the diff was built.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Path {
    /// Guided by the earlier map.
    Guided {
        /// Bytes of A compared against B' and not hashed.
        confirmed: Bytes,
        /// Bytes of A scanned.
        scanned: Bytes,
    },
    /// A fresh diff, for the reason given.
    Full(Fallback),
}

/// Why a guided run fell back to a fresh diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fallback {
    /// The skeleton half could refuse a fresh diff's scans, so its stratum
    /// cannot be shown to match, or the guided run's own working set did
    /// not fit.
    Budget,
}

/// The diff of `a` and `b`, guided by `guide`, taken from an earlier diff
/// of `a` and an earlier version of `b`. Its segments, hunks and unified
/// output are those of [`Diff::with_watch`] with the same arguments, when
/// the budget holds nothing else: the caller drops its copy of the earlier
/// map once the guide is built.
///
/// # Errors
///
/// As for [`Diff::with_watch`].
pub fn diff<
    A: Source + Send + Sync + ?Sized + 'static,
    B: Source + Send + Sync + ?Sized + 'static,
>(
    a: Arc<A>,
    b: Arc<B>,
    guide: Guide,
    budget: &Budget,
    threads: usize,
    config: Config,
    watch: &Watch,
) -> Result<(Diff<A, B>, Path)> {
    if watch.is_cancelled() {
        return Err(Error::Cancelled);
    }
    match guided(&*a, &*b, guide, budget, watch)? {
        Some((parts, path)) => Ok((
            Diff::from_parts(a, b, parts, budget, threads, config, watch)?,
            path,
        )),
        None => Ok((
            Diff::with_watch(a, b, budget, threads, config, watch)?,
            Path::Full(Fallback::Budget),
        )),
    }
}

/// Both scans and the skeleton, or `None` when the run must fall back.
fn guided<A: Source + Sync + ?Sized, B: Source + Sync + ?Sized>(
    a: &A,
    b: &B,
    mut guide: Guide,
    budget: &Budget,
    watch: &Watch,
) -> Result<Option<(Parts, Path)>> {
    let (len_a, len_b) = (a.len().0, b.len().0);
    let (cfg_a, cfg_b) = ScanConfig::for_pair(a.len(), b.len(), budget);
    let free = budget.available(Half::Skeleton).0;
    // A fresh diff scans A with the half free, then B' while A keeps its
    // samples. If neither can be refused, both strata follow the files.
    if scan::peak_bytes(len_a, &cfg_a) > free
        || scan::kept_bytes(len_a, &cfg_a) + scan::peak_bytes(len_b, &cfg_b) > free
    {
        return Ok(None);
    }
    let mut scan_b = scan::scan_metered(b, budget, &cfg_b, &Meter::new(watch, len_a))?;
    let iv = scan::checkpoint_interval(len_a, &cfg_a);
    let Some(compared) = refused_as_none(compare(a, b, &guide.pairs, iv, budget, watch))? else {
        return Ok(None);
    };
    let Some(mut scan_a) = refused_as_none(derive_a(
        a,
        b,
        &guide.pairs,
        &compared,
        &scan_b,
        &cfg_a,
        budget,
    ))?
    else {
        return Ok(None);
    };
    drop(compared.slots);
    drop(compared.held);
    let mut k = 0;
    guide.pairs.retain(|_| {
        k += 1;
        !compared.mismatch[k - 1]
    });
    drop(compared.mismatch);
    let confirmed: u64 = guide.pairs.iter().map(|p| p.len).sum();
    let skeleton = skeleton::build_watched(
        a,
        b,
        (&mut scan_a, &mut scan_b),
        budget,
        watch,
        len_a + len_b,
        &guide.pairs,
    )?;
    drop(guide);
    let path = Path::Guided {
        confirmed: Bytes(confirmed),
        scanned: Bytes(len_a - confirmed),
    };
    Ok(Some(((scan_a, scan_b, skeleton), path)))
}

/// Both scans and the map, as [`Diff::from_parts`] takes them.
type Parts = (Scanned, Scanned, Skeleton);

/// A budget refusal of the guided run's own working set, as a fallback.
fn refused_as_none<T>(r: Result<T>) -> Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(Error::Budget(_)) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The `Equal` ranges of an earlier map that still fit both files and
/// start and end at a line start on both sides, held in the Refine half,
/// 24 bytes per range.
#[derive(Debug)]
pub struct Guide {
    pairs: Vec<Confirmed>,
    _held: Reservation,
}

impl Guide {
    /// The guide for `a` and `b` from `prev`, the map of `a` and an earlier
    /// version of `b`, taken at the same offsets: what serves B' when it
    /// is B with lines appended.
    ///
    /// # Errors
    ///
    /// [`Error::Budget`] when the Refine half cannot hold it,
    /// [`Error::Io`] from a one-byte read of either file.
    pub fn new<A: Source + ?Sized, B: Source + ?Sized>(
        a: &A,
        b: &B,
        prev: &Previous<'_>,
        budget: &Budget,
    ) -> Result<Self> {
        let whole = [cdc::Run {
            old: 0,
            new: 0,
            bytes: prev.len_b.0,
        }];
        guide(a, b, prev, &whole, budget)
    }

    /// The guide for `a` and `b` from `prev`, with each `Equal` range moved
    /// to where `runs`, matched chunks of the earlier B and of `b`, put its
    /// bytes in `b`; the parts outside every run are left out.
    ///
    /// # Errors
    ///
    /// As for [`Guide::new`].
    pub fn translated<A: Source + ?Sized, B: Source + ?Sized>(
        a: &A,
        b: &B,
        prev: &Previous<'_>,
        runs: &cdc::Runs,
        budget: &Budget,
    ) -> Result<Self> {
        guide(a, b, prev, runs.runs(), budget)
    }
}

fn guide<A: Source + ?Sized, B: Source + ?Sized>(
    a: &A,
    b: &B,
    prev: &Previous<'_>,
    runs: &[cdc::Run],
    budget: &Budget,
) -> Result<Guide> {
    let (len_a, len_b) = (a.len().0, b.len().0);
    let mut held = budget.try_reserve(Half::Refine, Bytes(0))?;
    let mut pairs: Vec<Confirmed> = Vec::new();
    let end = |i: usize| match prev.starts.get(i + 1) {
        Some(&[x, y]) => (x.0, y.0),
        None => (prev.len_a.0, prev.len_b.0),
    };
    let mut r = 0;
    for (i, s) in prev.segments.iter().enumerate() {
        if s.status != SegmentStatus::Equal {
            continue;
        }
        let ([x, y], (ex, ey)) = (prev.starts[i], end(i));
        let (x, y) = (x.0, y.0);
        let len = ex.saturating_sub(x);
        if len == 0 || ey.saturating_sub(y) != len {
            continue;
        }
        while r < runs.len() && runs[r].old + runs[r].bytes <= y {
            r += 1;
        }
        for run in runs[r..].iter().take_while(|run| run.old < y + len) {
            let from = y.max(run.old);
            let to = (y + len).min(run.old + run.bytes);
            if from >= to {
                continue;
            }
            let p = Confirmed {
                a: x + (from - y),
                b: from - run.old + run.new,
                len: to - from,
            };
            if p.a + p.len > len_a || p.b + p.len > len_b || !lines_up(a, b, p, len_a, len_b)? {
                continue;
            }
            if pairs.len() == pairs.capacity() {
                let more = pairs.capacity().max(16);
                held.grow(Bytes(more as u64 * CONFIRMED_BYTES))?;
                pairs.try_reserve_exact(more).map_err(|_| oom())?;
            }
            pairs.push(p);
        }
    }
    held.shrink(Bytes(
        (pairs.capacity() - pairs.len()) as u64 * CONFIRMED_BYTES,
    ));
    pairs.shrink_to_fit();
    Ok(Guide { pairs, _held: held })
}

/// Whether range `p` starts at a line start in both files and ends at
/// one, or at the end of both.
fn lines_up<A: Source + ?Sized, B: Source + ?Sized>(
    a: &A,
    b: &B,
    p: Confirmed,
    len_a: u64,
    len_b: u64,
) -> Result<bool> {
    let ends =
        byte_at(a, p.a + p.len - 1)? == b'\n' || (p.a + p.len == len_a && p.b + p.len == len_b);
    Ok(ends
        && (p.a == 0 || byte_at(a, p.a - 1)? == b'\n')
        && (p.b == 0 || byte_at(b, p.b - 1)? == b'\n'))
}

// Three one-byte reads per range; read them with the range's
// first and last blocks if a map with many ranges shows them in a profile.
fn byte_at<S: Source + ?Sized>(src: &S, at: u64) -> Result<u8> {
    let mut b = [0u8; 1];
    src.read_exact_at(&mut b, ByteOffset(at))?;
    Ok(b[0])
}

fn to_usize(n: u64) -> usize {
    usize::try_from(n).expect("count fits usize on 64-bit targets")
}

fn oom() -> Error {
    std::io::Error::from(std::io::ErrorKind::OutOfMemory).into()
}

/// Bytes `lo..hi` of guide range `pair`, relative to its start.
#[derive(Debug, Clone, Copy)]
struct Unit {
    pair: usize,
    lo: u64,
    hi: u64,
}

/// What comparing the guide found.
struct Compared {
    units: Vec<Unit>,
    /// Newlines in each unit.
    newlines: Vec<u64>,
    /// Per unit, where its checkpoint slots start in `slots`, and how many
    /// it filled.
    filled: Vec<(usize, usize)>,
    /// The first line start in each block of A that a unit saw, with the
    /// unit-relative line count of its range; see [`compare`].
    slots: Vec<Checkpoint>,
    /// Per guide range: some byte differs.
    mismatch: Vec<bool>,
    held: Reservation,
}

/// A unit for a lane: its index and where its answers go.
type LaneItem<'s> = (usize, &'s mut [Checkpoint], &'s mut u64, &'s mut bool);

/// Compares every guide range, A against B', on up to [`MAX_THREADS`]
/// lanes, and records the first line start of every A block of `iv` bytes
/// inside each unit.
fn compare<A: Source + Sync + ?Sized, B: Source + Sync + ?Sized>(
    a: &A,
    b: &B,
    pairs: &[Confirmed],
    iv: u64,
    budget: &Budget,
    watch: &Watch,
) -> Result<Compared> {
    let count: usize = pairs.iter().map(|p| to_usize(p.len.div_ceil(UNIT))).sum();
    // A unit sees at most this many block starts: its bytes over the
    // interval, plus the blocks its two ends fall in.
    let slot_count = |len: u64| to_usize(len / iv + 2);
    let slots_total: usize = pairs
        .iter()
        .flat_map(|p| units_of(p.len).map(move |(lo, hi)| slot_count(hi - lo)))
        .sum();
    let table = count as u64 * (UNIT_BYTES + UNIT_WORK_BYTES)
        + slots_total as u64 * CHECKPOINT_BYTES
        + pairs.len() as u64
        // The lanes' lists and rayon's collection of their answers.
        + 4096;
    let mut held = budget.try_reserve(Half::Refine, Bytes(table))?;
    let lanes = count.clamp(1, MAX_THREADS);
    let free = budget.available(Half::Refine).0;
    let buf = (free / (2 * 2 * lanes as u64)).clamp(MIN_BUF, MAX_BUF);
    held.grow(Bytes(2 * buf * lanes as u64))?;

    let mut units = Vec::with_capacity(count);
    let mut filled = Vec::with_capacity(count);
    let mut at = 0;
    for (i, p) in pairs.iter().enumerate() {
        for (lo, hi) in units_of(p.len) {
            units.push(Unit { pair: i, lo, hi });
            filled.push((at, 0));
            at += slot_count(hi - lo);
        }
    }
    let mut slots = vec![
        Checkpoint {
            line: LineNo(0),
            offset: ByteOffset(0),
        };
        slots_total
    ];
    let mut newlines = vec![0u64; count];
    let mut bad = vec![false; count];

    // Deal units round-robin to the lanes, each with its own outputs.
    let mut lane_work: Vec<Vec<LaneItem<'_>>> = (0..lanes)
        .map(|_| Vec::with_capacity(count.div_ceil(lanes)))
        .collect();
    let mut rest: &mut [Checkpoint] = &mut slots;
    for (k, ((nl, bad), u)) in newlines.iter_mut().zip(&mut bad).zip(&units).enumerate() {
        let (mine, tail) = rest.split_at_mut(slot_count(u.hi - u.lo));
        rest = tail;
        lane_work[k % lanes].push((k, mine, nl, bad));
    }
    let used: Vec<Vec<(usize, usize)>> = lane_work
        .into_par_iter()
        .map(|work| -> Result<Vec<(usize, usize)>> {
            let (mut buf_a, mut buf_b) = (vec![0u8; to_usize(buf)], vec![0u8; to_usize(buf)]);
            let mut used = Vec::with_capacity(work.len());
            for (k, out, nl, bad) in work {
                let u = units[k];
                let p = pairs[u.pair];
                let r = compare_unit(a, b, p, u, iv, (&mut buf_a, &mut buf_b), out, watch)?;
                *nl = r.newlines;
                *bad = r.mismatch;
                used.push((k, r.filled));
            }
            Ok(used)
        })
        .collect::<Result<_>>()?;
    for (k, n) in used.into_iter().flatten() {
        filled[k].1 = n;
    }
    held.shrink(Bytes(2 * buf * lanes as u64));
    let mut mismatch = vec![false; pairs.len()];
    for (u, &b) in units.iter().zip(&bad) {
        mismatch[u.pair] |= b;
    }
    Ok(Compared {
        units,
        newlines,
        filled,
        slots,
        mismatch,
        held,
    })
}

fn units_of(len: u64) -> impl Iterator<Item = (u64, u64)> {
    (0..len.div_ceil(UNIT)).map(move |i| (i * UNIT, ((i + 1) * UNIT).min(len)))
}

struct UnitResult {
    newlines: u64,
    mismatch: bool,
    filled: usize,
}

/// Compares one unit and records, in `out`, the first line start of
/// every A block it sees. A line start at relative offset `r > 0` belongs
/// to the unit holding the newline before it, and gets the count of
/// newlines in the unit up to and including that one; the range's own
/// start belongs to its first unit, with count 0.
#[allow(
    clippy::too_many_arguments,
    reason = "one call site, all inputs distinct"
)]
fn compare_unit<A: Source + ?Sized, B: Source + ?Sized>(
    a: &A,
    b: &B,
    p: Confirmed,
    u: Unit,
    iv: u64,
    (buf_a, buf_b): (&mut [u8], &mut [u8]),
    out: &mut [Checkpoint],
    watch: &Watch,
) -> Result<UnitResult> {
    let mut r = UnitResult {
        newlines: 0,
        mismatch: false,
        filled: 0,
    };
    let mut last_block = u64::MAX;
    let mut record = |line: u64, at: u64, r: &mut UnitResult| {
        if at / iv != last_block {
            last_block = at / iv;
            out[r.filled] = Checkpoint {
                line: LineNo(line),
                offset: ByteOffset(at),
            };
            r.filled += 1;
        }
    };
    if u.lo == 0 {
        record(0, p.a, &mut r);
    }
    let mut pos = u.lo;
    while pos < u.hi {
        if watch.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let n = to_usize((u.hi - pos).min(buf_a.len() as u64));
        a.read_exact_at(&mut buf_a[..n], ByteOffset(p.a + pos))?;
        b.read_exact_at(&mut buf_b[..n], ByteOffset(p.b + pos))?;
        if buf_a[..n] != buf_b[..n] {
            r.mismatch = true;
            return Ok(r);
        }
        for nl in memchr_iter(b'\n', &buf_a[..n]) {
            r.newlines += 1;
            let rel = pos + nl as u64 + 1;
            if rel < p.len {
                record(r.newlines, p.a + rel, &mut r);
            }
        }
        pos += n as u64;
    }
    Ok(r)
}

/// A's scan, as [`scan::scan`] with `cfg` would give it once thinned to
/// at least B''s stratum: samples from B' in the confirmed ranges and
/// from A's own lines elsewhere, checkpoints from the comparison and from
/// the scanned ranges.
fn derive_a<A: Source + ?Sized, B: Source + ?Sized>(
    a: &A,
    b: &B,
    pairs: &[Confirmed],
    compared: &Compared,
    scan_b: &Scanned,
    cfg: &ScanConfig,
    budget: &Budget,
) -> Result<Scanned> {
    let len_a = a.len().0;
    let iv = scan::checkpoint_interval(len_a, cfg);
    let blocks = to_usize(len_a.div_ceil(iv));
    let mut keep = budget.try_reserve(Half::Skeleton, Bytes(scan::kept_bytes(len_a, cfg)))?;
    let mut out = Derived {
        samples: Vec::new(),
        cap: cfg.sample_cap,
        level: scan_b.level(),
        checkpoints: Vec::new(),
        iv,
        line: 0,
        last_block: u64::MAX,
    };
    out.samples
        .try_reserve_exact(cfg.sample_cap)
        .map_err(|_| oom())?;
    out.checkpoints
        .try_reserve_exact(blocks)
        .map_err(|_| oom())?;
    // The region reader's buffer, in the Refine half like the comparison's.
    let _buf_held = budget.try_reserve(Half::Refine, Bytes(MAX_BUF))?;
    let mut buf = vec![0u8; to_usize(MAX_BUF)];

    let samples_b = scan_b.samples();
    let mut at = 0;
    let mut k = 0;
    for (i, p) in pairs.iter().enumerate() {
        let first = k;
        while k < compared.units.len() && compared.units[k].pair == i {
            k += 1;
        }
        if compared.mismatch[i] {
            continue;
        }
        out.scan_region(a, at, p.a, &mut buf)?;
        let base = out.line;
        // Samples: B''s, moved to A.
        let line_b = line_at(b, scan_b.index(), p.b, &mut buf)?;
        let lo = samples_b.partition_point(|s| s.offset.0 < p.b);
        let hi = samples_b.partition_point(|s| s.offset.0 < p.b + p.len);
        for s in &samples_b[lo..hi] {
            out.push_sample(Sample {
                hash: s.hash,
                line: LineNo(base + (s.line.0 - line_b)),
                offset: ByteOffset(s.offset.0 - p.b + p.a),
            })?;
        }
        // Checkpoints, unit by unit.
        let mut unit_base = 0;
        for u in first..k {
            let (from, n) = compared.filled[u];
            for c in &compared.slots[from..from + n] {
                out.push_checkpoint(base + unit_base + c.line.0, c.offset.0);
            }
            unit_base += compared.newlines[u];
        }
        let ends_open = p.len > 0 && byte_at(a, p.a + p.len - 1)? != b'\n';
        out.line = base + unit_base + u64::from(ends_open);
        at = p.a + p.len;
    }
    out.scan_region(a, at, len_a, &mut buf)?;
    drop(buf);

    let Derived {
        mut samples,
        level,
        mut checkpoints,
        line,
        ..
    } = out;
    samples.shrink_to_fit();
    checkpoints.shrink_to_fit();
    let held =
        samples.capacity() as u64 * SAMPLE_BYTES + checkpoints.capacity() as u64 * CHECKPOINT_BYTES;
    keep.shrink(Bytes(keep.bytes().0 - held));
    let index = LineIndex::new(checkpoints, line, Bytes(iv));
    Ok(Scanned::from_parts(level, samples, index, keep))
}

/// The line number of B' at byte `at`, a line start: from the checkpoint
/// before it, counting newlines.
fn line_at<S: Source + ?Sized>(src: &S, index: &LineIndex, at: u64, buf: &mut [u8]) -> Result<u64> {
    let cps = index.checkpoints();
    let i = cps.partition_point(|c| c.offset.0 <= at);
    let Some(c) = i.checked_sub(1).map(|i| cps[i]) else {
        return Ok(0);
    };
    let (mut line, mut pos) = (c.line.0, c.offset.0);
    while pos < at {
        let n = to_usize((at - pos).min(buf.len() as u64));
        src.read_exact_at(&mut buf[..n], ByteOffset(pos))?;
        line += memchr_iter(b'\n', &buf[..n]).count() as u64;
        pos += n as u64;
    }
    Ok(line)
}

/// A's samples and checkpoints as they are produced, in file order.
struct Derived {
    samples: Vec<Sample>,
    cap: usize,
    level: u32,
    checkpoints: Vec<Checkpoint>,
    iv: u64,
    /// Lines of A so far.
    line: u64,
    last_block: u64,
}

impl Derived {
    /// Keeps `s` if it is in the current stratum, raising the stratum as
    /// a scan does when the samples would pass their cap.
    fn push_sample(&mut self, s: Sample) -> Result<()> {
        if !strata::contains(self.level, s.hash) {
            return Ok(());
        }
        while self.samples.len() >= self.cap {
            if self.level == strata::MAX_LEVEL {
                // A scan of A refuses too; the fresh diff reports it.
                return Err(Error::Budget(crate::budget::BudgetExceeded {
                    half: Half::Skeleton,
                    requested: Bytes((self.cap as u64 + 1) * SAMPLE_BYTES),
                    available: Bytes(self.cap as u64 * SAMPLE_BYTES),
                }));
            }
            self.level += 1;
            strata::thin(&mut self.samples, self.level);
            if !strata::contains(self.level, s.hash) {
                return Ok(());
            }
        }
        self.samples.push(s);
        Ok(())
    }

    /// Records a line start of A if it is the first in its block.
    fn push_checkpoint(&mut self, line: u64, at: u64) {
        let block = at / self.iv;
        if self.last_block == u64::MAX || block > self.last_block {
            self.last_block = block;
            self.checkpoints.push(Checkpoint {
                line: LineNo(line),
                offset: ByteOffset(at),
            });
        }
    }

    /// Scans A from `from`, a line start, to `to`, a line start or the
    /// end of A, as the scanner would.
    fn scan_region<S: Source + ?Sized>(
        &mut self,
        src: &S,
        from: u64,
        to: u64,
        buf: &mut [u8],
    ) -> Result<()> {
        let mut pos = from;
        let mut line_start = from;
        let mut filled = 0usize;
        let mut long: Option<Xxh3> = None;
        loop {
            let at = pos + filled as u64;
            let want = to_usize(((buf.len() - filled) as u64).min(to - at));
            if want == 0 {
                if filled > 0 || long.is_some() {
                    self.line(&buf[..filled], line_start, long.take())?;
                }
                return Ok(());
            }
            src.read_exact_at(&mut buf[filled..filled + want], ByteOffset(at))?;
            let end = filled + want;
            let mut start = 0;
            for nl in memchr_iter(b'\n', &buf[filled..end]).map(|k| k + filled) {
                self.line(&buf[start..=nl], line_start, long.take())?;
                start = nl + 1;
                line_start = pos + start as u64;
            }
            if start == 0 && end == buf.len() {
                long.get_or_insert_default().update(&buf[..end]);
                pos += end as u64;
                filled = 0;
            } else {
                buf.copy_within(start..end, 0);
                filled = end - start;
                pos += start as u64;
            }
        }
    }

    fn line(&mut self, tail: &[u8], at: u64, long: Option<Xxh3>) -> Result<()> {
        let hash = match long {
            Some(mut h) => {
                h.update(tail);
                h.digest()
            }
            None => xxh3_64(tail),
        };
        self.push_checkpoint(self.line, at);
        self.push_sample(Sample {
            hash,
            line: LineNo(self.line),
            offset: ByteOffset(at),
        })?;
        self.line += 1;
        Ok(())
    }
}
