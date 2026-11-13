//! Parallel line scanner: line ends, hashes and sample records.
//!
//! One pass over a [`Source`] yields the line count, the [`Sample`] records
//! of one stratum and a sparse [`LineIndex`]. The source is cut into chunks
//! of `chunk` bytes; a chunk owns every line that starts inside it, however
//! long. Workers take chunks in file order from a shared counter, so reads
//! stay nearly sequential, and each worker keeps one fixed read buffer and
//! two small staging vectors for the whole scan. Lines are numbered within
//! their chunk until all counts are known; a prefix sum over chunks then
//! makes the numbers absolute.
//!
//! The sample and checkpoint vectors start at what the file is expected to
//! produce, never more than the configured caps, and grow toward the caps
//! with a reservation taken first. When the caps are reached, or the budget
//! refuses to grow, the stratum is raised, or the checkpoint interval
//! doubled, and the vectors are thinned in place; nothing is reread.
//!
//! Checkpoints are first taken every `interval` lines of each chunk, from
//! its first line, since a worker does not know where its chunk starts in
//! lines. Once the scan ends, the index keeps the last of them at or
//! before each multiple of `interval`, so its size depends only on the line
//! count, never on the chunk size or the thread count, and consecutive
//! checkpoints are fewer than `2 * interval` lines apart. While the scan
//! runs, the cap has room for one more checkpoint per chunk.
//!
//! Memory, all from the skeleton half: the sample and checkpoint vectors for
//! the lifetime of the result, and for the duration of the scan only, the
//! per-worker buffers, at most one chunk plus a tail and never more than the
//! file, one counter per chunk and room for one checkpoint per chunk. Nothing is sized from the budget alone:
//! a small file costs little whatever `--mem` says.
//!
//! A [`Watch`] follows the scan from another thread: before every read the
//! workers check whether it was cancelled, and after every chunk they count
//! the chunk's bytes in it.

use std::num::NonZero;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;

use memchr::memchr_iter;
use rayon::slice::ParallelSliceMut;
use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use crate::budget::{Budget, BudgetExceeded, Half, Reservation};
use crate::error::{Error, Result};
use crate::index::{Checkpoint, LineIndex, read_retrying};
use crate::sample::Sample;
use crate::source::Source;
use crate::strata;
use crate::types::{ByteOffset, Bytes, LineNo};

/// Extra bytes read past a chunk's nominal end, so the line that crosses
/// the boundary usually arrives with the first read.
const TAIL: usize = 64 << 10;
/// Samples a worker holds before appending to the shared vector.
const STAGING_SAMPLES: usize = 4 << 10;
/// Checkpoints a worker holds before appending to the shared vector.
const STAGING_CHECKPOINTS: usize = 1 << 10;
const SAMPLE_BYTES: u64 = size_of::<Sample>() as u64;
const CHECKPOINT_BYTES: u64 = size_of::<Checkpoint>() as u64;
/// Bytes per line assumed before a file is read. Dumps and CSV exports run
/// from about 80 bytes to a few kilobytes per line; guessing low errs on
/// the side of memory, and in-place thinning covers the rest. The price is
/// paid on long lines: strata only coarsen, so a file of 3.5 KB lines
/// starts about six strata above what the half could hold and keeps that
/// much sample capacity unused.
const PESSIMISTIC_BYTES_PER_LINE: u64 = 64;
/// Heap the anchor stage needs per sample beyond the record itself: its
/// occurrence-counting table. The stratum leaves room for it.
const ANCHOR_TABLE_BYTES_PER_SAMPLE: u64 = 16;
/// Capacity every file gets before the rest is split by length.
const MIN_SAMPLES: u64 = 1 << 12;
const MIN_CHECKPOINTS: u64 = 1 << 10;
const MIN_INTERVAL: u64 = 16;
/// Past this interval the checkpoints are one per chunk and thinning
/// cannot help; the file is beyond what the index can describe.
const MAX_INTERVAL: u64 = 1 << 40;

fn to_usize(n: u64) -> usize {
    usize::try_from(n).expect("count fits usize on 64-bit targets")
}

fn estimated_lines(len: Bytes) -> u64 {
    len.0 / PESSIMISTIC_BYTES_PER_LINE + 1
}

/// Sizes for one scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanConfig {
    /// Stratum to sample at; may be raised during the scan.
    pub level: u32,
    /// Most samples kept; reaching it raises the stratum.
    pub sample_cap: usize,
    /// Lines between checkpoints, at least one; may be doubled.
    pub checkpoint_interval: u64,
    /// Most checkpoints kept; reaching it doubles the interval.
    pub checkpoint_cap: usize,
    /// Bytes per chunk; each worker's buffer is this plus a small tail.
    pub chunk: Bytes,
    /// Worker threads, at most one per chunk.
    pub threads: usize,
}

impl ScanConfig {
    /// Sizes the scans of a pair of files so that both samples, the anchor
    /// table and both indexes fit the skeleton half together, at one
    /// shared stratum.
    ///
    /// The half is split as 1/16 for the indexes, the rest for samples
    /// and the anchor table; every file gets a floor, then a share of the
    /// remainder proportional to its length. Read buffers take 1/4 of the
    /// half while a scan runs and are gone before the anchor table is
    /// built. The thread count is the core count: one thread hashes short
    /// lines at about 1 GB/s, under a third of an SSD, so the scan is
    /// CPU-bound until several cores share it.
    #[must_use]
    pub fn for_pair(len_a: Bytes, len_b: Bytes, budget: &Budget) -> (Self, Self) {
        let half = budget.capacity(Half::Skeleton).0;
        let cores = thread::available_parallelism().map_or(1, NonZero::get) as u64;
        let pool = half / 4;
        let chunk = (pool / cores).clamp(Bytes::mib(4).0.min(pool.max(1 << 16)), Bytes::mib(16).0);
        let threads = (pool / chunk).clamp(1, cores);
        let index_share = half / 16;
        let sample_share = half - index_share;
        let samples = sample_share / (SAMPLE_BYTES + ANCHOR_TABLE_BYTES_PER_SAMPLE);
        let spare_samples = samples.saturating_sub(2 * MIN_SAMPLES);
        let spare_checkpoints =
            (index_share / CHECKPOINT_BYTES).saturating_sub(2 * MIN_CHECKPOINTS);
        let total = len_a.0.saturating_add(len_b.0);
        let level = strata::initial_level(estimated_lines(Bytes(total)), samples);
        let file = |len: Bytes| {
            let share = |n: u64| {
                u64::try_from(u128::from(n) * u128::from(len.0) / u128::from(total.max(1)))
                    .unwrap_or(n)
            };
            let checkpoint_cap = MIN_CHECKPOINTS + share(spare_checkpoints);
            Self {
                level,
                sample_cap: to_usize(MIN_SAMPLES + share(spare_samples)),
                checkpoint_interval: (estimated_lines(len) / checkpoint_cap + 1).max(MIN_INTERVAL),
                checkpoint_cap: to_usize(checkpoint_cap),
                chunk: Bytes(chunk),
                threads: to_usize(threads),
            }
        };
        (file(len_a), file(len_b))
    }
}

/// Follows a scan from another thread and stops it.
///
/// [`build_pair_watched`](crate::skeleton::build_pair_watched) and
/// [`Diff::with_watch`](crate::diff::Diff::with_watch) count in it the bytes
/// their two passes over the files have covered, of twice the size of both
/// files, and check it before every read. Once it is cancelled, the scan
/// returns [`Error::Cancelled`] when the reads already under way end: at
/// most one chunk per scan thread in the first pass, one comparison buffer
/// per lane in the second. Holds no heap.
#[derive(Debug, Default)]
pub struct Watch {
    cancelled: AtomicBool,
    done: AtomicU64,
}

impl Watch {
    /// A watch on a scan that has not started.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Asks the scan to stop at its next read.
    pub fn cancel(&self) {
        // Relaxed: the flag orders no other memory; the workers only need
        // to see it eventually, and they look before every read.
        self.cancelled.store(true, Ordering::Relaxed);
    }

    /// Whether [`cancel`](Self::cancel) was called.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }

    /// Bytes both passes have covered so far. Only grows, and reaches twice
    /// the size of both files when the scan ends.
    #[must_use]
    pub fn done(&self) -> Bytes {
        Bytes(self.done.load(Ordering::Relaxed))
    }
}

/// The part of a [`Watch`] one stage of the scan fills: its bytes count from
/// `base`. It only ever raises the watch's figure, so a stage run again
/// after a budget retry does not move the progress back.
#[derive(Debug)]
pub(crate) struct Meter<'w> {
    watch: &'w Watch,
    base: u64,
    local: AtomicU64,
}

impl<'w> Meter<'w> {
    pub(crate) fn new(watch: &'w Watch, base: u64) -> Self {
        Self {
            watch,
            base,
            local: AtomicU64::new(0),
        }
    }

    /// Counts `n` more bytes of this stage.
    pub(crate) fn add(&self, n: u64) {
        // Relaxed: a progress figure, read for display only.
        let local = self.local.fetch_add(n, Ordering::Relaxed) + n;
        self.watch
            .done
            .fetch_max(self.base + local, Ordering::Relaxed);
    }

    /// [`Error::Cancelled`] once the watch is cancelled.
    pub(crate) fn check(&self) -> Result<()> {
        if self.watch.is_cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// What one scan produced. Dropping it releases the reservation.
#[derive(Debug)]
pub struct Scanned {
    level: u32,
    samples: Vec<Sample>,
    index: LineIndex,
    /// Skeleton-half bytes held for `samples` and `index`; kept private so
    /// the vectors cannot outlive what pays for them.
    reservation: Reservation,
}

impl Scanned {
    /// The stratum the samples belong to, at least the configured one.
    #[must_use]
    pub fn level(&self) -> u32 {
        self.level
    }

    /// Samples of that stratum, in line order.
    #[must_use]
    pub fn samples(&self) -> &[Sample] {
        &self.samples
    }

    /// The samples, for sorting or marking in place.
    pub fn samples_mut(&mut self) -> &mut [Sample] {
        &mut self.samples
    }

    /// Line count and sparse line starts.
    #[must_use]
    pub fn index(&self) -> &LineIndex {
        &self.index
    }

    /// Skeleton-half bytes this result holds.
    #[must_use]
    pub fn held(&self) -> Bytes {
        self.reservation.bytes()
    }

    /// Raises the stratum to `level`, thinning the samples in place, and
    /// gives the freed bytes back to the budget.
    ///
    /// # Panics
    ///
    /// Panics if `level` is below the current one: strata only coarsen.
    pub fn thin(&mut self, level: u32) {
        assert!(level >= self.level, "stratum {level} below {}", self.level);
        strata::thin(&mut self.samples, level);
        self.level = level;
        release_slack(&mut self.samples, &mut self.reservation);
    }
}

impl Scanned {
    /// The index alone, with the reservation shrunk to what it holds. For a
    /// diff that is past the skeleton and only reads lines.
    #[must_use]
    pub fn into_index(self) -> (LineIndex, Reservation) {
        let Scanned {
            samples,
            index,
            mut reservation,
            ..
        } = self;
        let bytes = (samples.capacity() as u64) * SAMPLE_BYTES;
        drop(samples);
        reservation.shrink(Bytes(bytes));
        (index, reservation)
    }
}

/// Drops spare capacity and gives those bytes back to the reservation.
pub(crate) fn release_slack<T>(v: &mut Vec<T>, reservation: &mut Reservation) {
    let before = v.capacity();
    v.shrink_to_fit();
    reservation.shrink(Bytes(((before - v.capacity()) * size_of::<T>()) as u64));
}

/// Scans `source` once. `O(bytes)` time, memory as described in the
/// module docs.
///
/// # Errors
///
/// [`Error::Budget`] if the skeleton half cannot hold the configured
/// capacities and buffers, or if even the coarsest stratum or interval
/// overflows them; [`Error::Io`] from the source.
///
/// # Panics
///
/// Panics if `cfg.chunk` or `cfg.checkpoint_interval` is zero.
pub fn scan<S: Source + ?Sized>(source: &S, budget: &Budget, cfg: &ScanConfig) -> Result<Scanned> {
    scan_metered(source, budget, cfg, &Meter::new(&Watch::new(), 0))
}

/// [`scan`] that counts its chunks in `meter` and stops when its watch is
/// cancelled.
pub(crate) fn scan_metered<S: Source + ?Sized>(
    source: &S,
    budget: &Budget,
    cfg: &ScanConfig,
    meter: &Meter<'_>,
) -> Result<Scanned> {
    assert!(cfg.chunk.0 > 0, "chunk size must be positive");
    assert!(
        cfg.checkpoint_interval > 0,
        "checkpoint interval must be positive"
    );
    let len = source.len().0;
    let chunk = cfg.chunk.0;
    let chunks = len.div_ceil(chunk);
    let threads = cfg.threads.clamp(1, to_usize(chunks.max(1)));
    // A chunk is read from one byte before its start, so a buffer of
    // `len + 1` holds any chunk of the file whole and never fills.
    let buf_len = (chunk + TAIL as u64).min(len + 1);

    // Start at what the file is expected to produce; grow toward the caps.
    let lines = estimated_lines(Bytes(len));
    let sample_start = (lines >> cfg.level.min(63))
        .saturating_add(1)
        .min(cfg.sample_cap as u64);
    let checkpoint_start = (lines / cfg.checkpoint_interval)
        .saturating_add(chunks + 1)
        .min(cfg.checkpoint_cap as u64);
    let keep = budget.try_reserve(
        Half::Skeleton,
        Bytes(sample_start * SAMPLE_BYTES + checkpoint_start * CHECKPOINT_BYTES),
    )?;
    let per_worker = buf_len
        + STAGING_SAMPLES as u64 * SAMPLE_BYTES
        + STAGING_CHECKPOINTS as u64 * CHECKPOINT_BYTES;
    let work = budget.try_reserve(
        Half::Skeleton,
        Bytes(threads as u64 * per_worker + chunks * 8),
    )?;

    let shared = Shared {
        samples: Mutex::new(Vec::with_capacity(to_usize(sample_start))),
        sample_cap: cfg.sample_cap,
        level: AtomicU32::new(cfg.level),
        checkpoints: Mutex::new(Vec::with_capacity(to_usize(checkpoint_start))),
        checkpoint_cap: cfg.checkpoint_cap,
        chunk_starts: to_usize(chunks),
        interval: AtomicU64::new(cfg.checkpoint_interval),
        keep: Mutex::new(keep),
        buf_len: to_usize(buf_len),
        counts: (0..chunks).map(|_| AtomicU64::new(0)).collect(),
        next: AtomicU64::new(0),
        failed: AtomicBool::new(false),
        len,
        chunk,
        meter,
    };
    thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| s.spawn(|| Worker::new(&shared).run(source)))
            .collect();
        workers.into_iter().try_for_each(|w| {
            w.join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
        })
    })?;
    drop(work);

    // Each chunk's counter becomes the number of lines before it.
    let mut total = 0u64;
    for c in &shared.counts {
        let n = c.load(Ordering::Relaxed);
        c.store(total, Ordering::Relaxed);
        total += n;
    }
    let mut keep = shared
        .keep
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    let base =
        |offset: ByteOffset| shared.counts[to_usize(offset.0 / chunk)].load(Ordering::Relaxed);

    let level = shared.level.load(Ordering::Relaxed);
    let mut samples = shared
        .samples
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    debug_assert!(samples.iter().all(|s| strata::contains(level, s.hash)));
    for s in &mut samples {
        s.line += base(s.offset);
    }
    // Workers append in arbitrary chunk order, so the runs interleave; a
    // serial sort costs more than the parallel pass on small pairs.
    samples.par_sort_unstable_by_key(|s| s.line);

    let interval = shared.interval.load(Ordering::Relaxed);
    let mut checkpoints = shared
        .checkpoints
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    debug_assert!(
        checkpoints
            .iter()
            .all(|c| c.line.0.is_multiple_of(interval))
    );
    for c in &mut checkpoints {
        c.line += base(c.offset);
    }
    checkpoints.sort_unstable_by_key(|c| c.line);
    let interval = one_per_multiple(&mut checkpoints, total, interval, cfg.checkpoint_cap)?;

    release_slack(&mut samples, &mut keep);
    release_slack(&mut checkpoints, &mut keep);

    Ok(Scanned {
        level,
        samples,
        index: LineIndex::new(checkpoints, total, interval),
        reservation: keep,
    })
}

/// Thins `checkpoints`, sorted and absolute, of a file of `total` lines to
/// one per multiple of the interval, doubling `interval` until that fits
/// `cap`, and returns the interval.
fn one_per_multiple(
    checkpoints: &mut Vec<Checkpoint>,
    total: u64,
    mut interval: u64,
    cap: usize,
) -> Result<u64> {
    // Unless the budget refused it room, the scan doubled the interval only
    // while its checkpoints, at most `total / interval` plus one per chunk,
    // overflowed the cap plus one per chunk, so `total / interval` alone
    // overflowed the cap too: the interval reached here is the smallest
    // that fits, whatever the chunks.
    while total.div_ceil(interval) > cap as u64 {
        if interval >= MAX_INTERVAL {
            return Err(exceeded(to_usize(total.div_ceil(interval)), checkpoints));
        }
        interval *= 2;
    }
    // Keep the last checkpoint at or before each multiple of `interval`
    // below the line count. No two multiples share one, since the scan left
    // fewer than `interval` lines after each: exactly one per multiple.
    let mut kept = 0;
    for i in 0..checkpoints.len() {
        let end = checkpoints.get(i + 1).map_or(total, |c| c.line.0);
        if checkpoints[i].line.0.div_ceil(interval) * interval < end {
            checkpoints[kept] = checkpoints[i];
            kept += 1;
        }
    }
    checkpoints.truncate(kept);
    Ok(interval)
}

/// State the workers share. Lines in `samples` and `checkpoints` are
/// chunk-local until the scan ends.
struct Shared<'m> {
    samples: Mutex<Vec<Sample>>,
    /// Most samples the vector may grow to before the stratum is raised.
    sample_cap: usize,
    level: AtomicU32,
    checkpoints: Mutex<Vec<Checkpoint>>,
    /// Checkpoints the index may keep.
    checkpoint_cap: usize,
    /// Chunks: each starts with a checkpoint the index may drop later, so
    /// the vector may hold this many past the cap before the interval is
    /// doubled.
    chunk_starts: usize,
    interval: AtomicU64,
    /// Bytes held for both vectors. Locked only while a vector's own lock is
    /// held, always in that order, so the two cannot deadlock.
    keep: Mutex<Reservation>,
    /// Per-worker read buffer size.
    buf_len: usize,
    counts: Vec<AtomicU64>,
    next: AtomicU64,
    failed: AtomicBool,
    len: u64,
    chunk: u64,
    meter: &'m Meter<'m>,
}

impl Shared<'_> {
    /// Appends `staging`, raising the stratum as needed, and returns the
    /// stratum now in force.
    fn push_samples(&self, staging: &mut Vec<Sample>) -> Result<u32> {
        let mut out = self.samples.lock().unwrap_or_else(PoisonError::into_inner);
        // Relaxed is enough: the level is only a hint between flushes, and
        // this lock orders every write to the vector.
        let mut level = self.level.load(Ordering::Relaxed);
        strata::thin(staging, level);
        while out.len() + staging.len() > out.capacity()
            && !self.grow(&mut out, staging.len(), self.sample_cap)
        {
            if level == strata::MAX_LEVEL {
                return Err(exceeded(out.len() + staging.len(), &out));
            }
            level += 1;
            strata::thin(&mut out, level);
            strata::thin(staging, level);
        }
        self.level.store(level, Ordering::Relaxed);
        out.append(staging);
        Ok(level)
    }

    /// The most checkpoints the vector may grow to while it needs room for
    /// `need`: the cap, and past it one more per chunk. Growth stops at
    /// the cap first, so a scan that fits the cap takes no more than it
    /// did before the extra room existed.
    fn checkpoint_room(&self, need: usize) -> usize {
        if need <= self.checkpoint_cap {
            self.checkpoint_cap
        } else {
            self.checkpoint_cap.saturating_add(self.chunk_starts)
        }
    }

    /// Appends `staging`, doubling the interval as needed, and returns the
    /// interval now in force.
    fn push_checkpoints(&self, staging: &mut Vec<Checkpoint>) -> Result<u64> {
        let mut out = self
            .checkpoints
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let mut interval = self.interval.load(Ordering::Relaxed);
        staging.retain(|c| c.line.0.is_multiple_of(interval));
        while out.len() + staging.len() > out.capacity() && {
            let room = self.checkpoint_room(out.len() + staging.len());
            !self.grow(&mut out, staging.len(), room)
        } {
            if interval >= MAX_INTERVAL {
                return Err(exceeded(out.len() + staging.len(), &out));
            }
            interval *= 2;
            out.retain(|c| c.line.0.is_multiple_of(interval));
            staging.retain(|c| c.line.0.is_multiple_of(interval));
        }
        self.interval.store(interval, Ordering::Relaxed);
        out.append(staging);
        Ok(interval)
    }

    /// Grows `out` so `extra` more items fit, doubling toward `cap`, with
    /// the bytes reserved first. False when the cap is reached or the
    /// budget or allocator refuses; the caller then thins instead.
    fn grow<T>(&self, out: &mut Vec<T>, extra: usize, cap: usize) -> bool {
        let need = out.len() + extra;
        let old = out.capacity();
        if need > cap {
            return false;
        }
        let new = need.max(old.saturating_mul(2)).min(cap);
        let bytes = |n: usize| Bytes((n * size_of::<T>()) as u64);
        let mut keep = self.keep.lock().unwrap_or_else(PoisonError::into_inner);
        if keep.grow(bytes(new - old)).is_err() {
            return false;
        }
        if out.try_reserve_exact(new - out.len()).is_err() {
            keep.shrink(bytes(new - old));
            return false;
        }
        // try_reserve_exact may round up; account for what was allocated.
        if let Some(more) = out.capacity().checked_sub(new).filter(|&m| m > 0)
            && keep.grow(bytes(more)).is_err()
        {
            out.shrink_to(new);
        }
        true
    }
}

/// Even the coarsest stratum or interval needs more than the capacity
/// the configuration allotted.
#[cold]
fn exceeded<T>(needed: usize, out: &[T]) -> Error {
    let bytes = |n: usize| Bytes((n * size_of::<T>()) as u64);
    Error::Budget(BudgetExceeded {
        half: Half::Skeleton,
        requested: bytes(needed),
        available: bytes(out.len()),
    })
}

/// One thread's view of the scan: its buffers and its copy of the
/// stratum and interval.
struct Worker<'a> {
    shared: &'a Shared<'a>,
    mask: u64,
    interval: u64,
    samples: Vec<Sample>,
    checkpoints: Vec<Checkpoint>,
    /// Streaming hash of a line longer than the buffer, between reads.
    long: Option<Xxh3>,
}

impl<'a> Worker<'a> {
    fn new(shared: &'a Shared<'a>) -> Self {
        Self {
            shared,
            mask: strata::mask(shared.level.load(Ordering::Relaxed)),
            interval: shared.interval.load(Ordering::Relaxed),
            samples: Vec::with_capacity(STAGING_SAMPLES),
            checkpoints: Vec::with_capacity(STAGING_CHECKPOINTS),
            long: None,
        }
    }

    fn run<S: Source + ?Sized>(mut self, source: &S) -> Result<()> {
        let mut buf = vec![0u8; self.shared.buf_len];
        let chunks = self.shared.counts.len() as u64;
        let result = loop {
            if self.shared.failed.load(Ordering::Relaxed) {
                break Ok(());
            }
            let i = self.shared.next.fetch_add(1, Ordering::Relaxed);
            if i >= chunks {
                break self.flush();
            }
            match self.scan_chunk(source, &mut buf, i) {
                Ok(lines) => {
                    self.shared.counts[to_usize(i)].store(lines, Ordering::Relaxed);
                    let start = i * self.shared.chunk;
                    let end = (start + self.shared.chunk).min(self.shared.len);
                    self.shared.meter.add(end - start);
                }
                Err(e) => break Err(e),
            }
        };
        if result.is_err() {
            self.shared.failed.store(true, Ordering::Relaxed);
        }
        result
    }

    fn flush(&mut self) -> Result<()> {
        if !self.samples.is_empty() {
            self.mask = strata::mask(self.shared.push_samples(&mut self.samples)?);
        }
        if !self.checkpoints.is_empty() {
            self.interval = self.shared.push_checkpoints(&mut self.checkpoints)?;
        }
        Ok(())
    }

    /// One finished line: `bytes` is its tail, or the whole line when no
    /// streaming prefix is pending.
    #[inline]
    fn line(&mut self, bytes: &[u8], offset: u64, local: u64) -> Result<()> {
        let hash = match self.long.take() {
            Some(mut h) => {
                h.update(bytes);
                h.digest()
            }
            None => xxh3_64(bytes),
        };
        if hash & self.mask == 0 {
            self.samples.push(Sample {
                hash,
                line: LineNo(local),
                offset: ByteOffset(offset),
            });
            if self.samples.len() == STAGING_SAMPLES {
                self.mask = strata::mask(self.shared.push_samples(&mut self.samples)?);
            }
        }
        if local.is_multiple_of(self.interval) {
            self.checkpoints.push(Checkpoint {
                line: LineNo(local),
                offset: ByteOffset(offset),
            });
            if self.checkpoints.len() == STAGING_CHECKPOINTS {
                self.interval = self.shared.push_checkpoints(&mut self.checkpoints)?;
            }
        }
        Ok(())
    }

    /// Hashes every line that starts in chunk `i` and returns how many.
    ///
    /// Reading starts one byte before the chunk so that a `\n` right at
    /// the boundary is seen; bytes up to and including the first `\n`
    /// belong to the previous chunk and are skipped. If no `\n` lies in
    /// `[start - 1, end - 1)`, no line starts in the chunk.
    fn scan_chunk<S: Source + ?Sized>(
        &mut self,
        source: &S,
        buf: &mut [u8],
        i: u64,
    ) -> Result<u64> {
        let nominal_start = i * self.shared.chunk;
        let nominal_end = (nominal_start + self.shared.chunk).min(self.shared.len);
        let mut skipping = i > 0;
        // `pos` is the file offset of `buf[0]`; `line_start` that of the
        // current line, which may lie before `pos` while streaming.
        let mut pos = nominal_start.saturating_sub(1);
        let mut line_start = pos;
        let mut filled = 0usize;
        let mut lines = 0u64;
        loop {
            self.shared.meter.check()?;
            let n = read_retrying(source, &mut buf[filled..], ByteOffset(pos + filled as u64))?;
            if n == 0 {
                // A pending streaming hash is a line too, even with an
                // empty tail: its length was a multiple of the buffer.
                if !skipping && (filled > 0 || self.long.is_some()) {
                    self.line(&buf[..filled], line_start, lines)?;
                    lines += 1;
                }
                return Ok(lines);
            }
            let end = filled + n;
            let mut start = 0usize;
            for nl in memchr_iter(b'\n', &buf[filled..end]).map(|k| k + filled) {
                if skipping {
                    skipping = false;
                } else {
                    self.line(&buf[start..=nl], line_start, lines)?;
                    lines += 1;
                }
                start = nl + 1;
                line_start = pos + start as u64;
                if line_start >= nominal_end {
                    return Ok(lines);
                }
            }
            if start == 0 && end == buf.len() {
                // No line end in a full buffer.
                if skipping {
                    if pos + end as u64 >= nominal_end {
                        return Ok(0);
                    }
                } else {
                    self.long.get_or_insert_default().update(&buf[..end]);
                }
                pos += end as u64;
                filled = 0;
            } else {
                buf.copy_within(start..end, 0);
                filled = end - start;
                pos += start as u64;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests that run a scan live in `tests/scan.rs`: they allocate buffers,
    // and the counting allocator's own tests need a quiet process.
    #[test]
    fn for_pair_sizes_a_large_pair_to_level_seven() {
        let budget = Budget::new(Bytes::mib(256));
        let (a, b) = ScanConfig::for_pair(Bytes(10 << 30), Bytes(10 << 30), &budget);
        assert_eq!(a, b);
        assert_eq!(a.level, 7);
        assert!(a.chunk >= Bytes::mib(4) && a.chunk <= Bytes::mib(16));
        assert!(a.threads >= 1);
        fits(&budget, &a, &b);
    }

    #[test]
    fn for_pair_fits_any_budget_and_ratio() {
        for mib in [1, 8, 64, 1024] {
            let budget = Budget::new(Bytes::mib(mib));
            for (la, lb) in [(0, 0), (1, 1 << 40), (1 << 35, 1 << 20)] {
                let (a, b) = ScanConfig::for_pair(Bytes(la), Bytes(lb), &budget);
                assert_eq!(a.level, b.level);
                fits(&budget, &a, &b);
            }
        }
    }

    /// Worst case while the second file is scanned: both files at full
    /// capacity plus the read buffers; afterwards, both at full capacity
    /// plus the anchor table.
    fn fits(budget: &Budget, a: &ScanConfig, b: &ScanConfig) {
        let half = budget.capacity(Half::Skeleton).0;
        let kept = |c: &ScanConfig| {
            c.sample_cap as u64 * SAMPLE_BYTES + c.checkpoint_cap as u64 * CHECKPOINT_BYTES
        };
        let table = |c: &ScanConfig| c.sample_cap as u64 * ANCHOR_TABLE_BYTES_PER_SAMPLE;
        let buffers = a.threads as u64 * (a.chunk.0 + TAIL as u64);
        let floors = 2 * (MIN_SAMPLES * SAMPLE_BYTES + MIN_CHECKPOINTS * CHECKPOINT_BYTES);
        if half >= floors + buffers {
            assert!(kept(a) + kept(b) + buffers <= half, "{half} {a:?} {b:?}");
        }
        assert!(kept(a) + kept(b) + table(a) + table(b) <= half.max(floors * 2));
    }
}
