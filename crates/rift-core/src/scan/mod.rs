//! Parallel line scanner: line ends, hashes and sample records.
//!
//! One pass over a [`Source`] yields the line count, the [`Sample`] records
//! of one stratum and a sparse [`LineIndex`]. The source is cut into chunks
//! of `chunk` bytes; a chunk owns every line that starts inside it, however
//! long. Workers take chunks in file order from a shared counter, so reads
//! stay nearly sequential, and each worker reads its chunks through one
//! fixed buffer, which may be smaller than a chunk, and keeps two small
//! staging vectors for the whole scan. Lines are numbered within their
//! chunk until all counts are known; a prefix sum over chunks then makes
//! the numbers absolute.
//!
//! The sample vector starts at what the file is expected to produce, never
//! more than the configured cap, and grows toward the cap with a
//! reservation taken first. When the cap is reached, or the budget refuses
//! to grow, the stratum is raised and the vector is thinned in place;
//! nothing is reread.
//!
//! The index keeps the first line start of every `interval`-byte block of
//! the file. The interval is the configured one, raised so the file's
//! blocks fit the checkpoint cap, so the checkpoint vector is sized once,
//! from the file's length. Chunks are rounded up to whole blocks, so the
//! worker that owns a block's chunk sees every line start in the block.
//! The index depends only on the file and the two settings, never on the
//! chunk size, the buffers or the thread count.
//!
//! Memory, all from the skeleton half: the sample and checkpoint vectors for
//! the lifetime of the result, and for the duration of the scan only, the
//! per-worker read buffers, never more than the file, and staging
//! vectors, and one counter per chunk. Nothing is sized from the budget
//! alone: a small file costs little whatever `--mem` says.
//!
//! A [`Watch`] follows the scan from another thread: before every read the
//! workers check whether it was cancelled, and after every chunk they count
//! the chunk's bytes in it.
//!
//! When a result is being saved, the scan also hashes every fixed-size
//! hash chunk of the file with xxh3-64 and writes each hash at its slot of
//! the result's chunk table (see [`ChunkTable`]). The worker whose chunk
//! holds the start of a hash chunk hashes all of it, reading past its own
//! chunk's end when the hash chunk crosses it, so the table does not
//! depend on the chunk size or the thread count. The hashes take no heap:
//! each is written as soon as its chunk is done.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::thread;

use memchr::memchr_iter;
use rayon::slice::ParallelSliceMut;
use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use crate::budget::{Budget, BudgetExceeded, Half, MAX_THREADS, Reservation};
use crate::error::{Error, Result};
use crate::index::{Checkpoint, LineIndex, read_retrying};
use crate::sample::Sample;
use crate::saved::ChunkTable;
use crate::source::Source;
use crate::strata;
use crate::types::{ByteOffset, Bytes, LineNo};

/// Bytes a worker reads past its chunk's end before it knows the line
/// that crosses the end is longer, so that line usually arrives with the
/// read that reaches the end.
const TAIL: u64 = 64 << 10;
/// The unit of work: small enough to share a file of tens of MiB among
/// the workers, large enough that a large file needs few counters.
const CHUNK: u64 = 4 << 20;
/// Read buffer per worker. The largest keeps a worker's buffer in its
/// core's share of the cache, so hashing reads what the copy from the page
/// cache just wrote; the smallest keeps one read per line on typical lines.
const MIN_BUFFER: u64 = 64 << 10;
const MAX_BUFFER: u64 = 256 << 10;
/// Samples a worker holds before appending to the shared vector.
const STAGING_SAMPLES: usize = 2 << 10;
/// Checkpoints a worker holds before appending to the shared vector: with
/// the blocks of at least 1 KiB that `for_pair` sets, a full buffer brings
/// at most this many.
const STAGING_CHECKPOINTS: usize = 256;
const SAMPLE_BYTES: u64 = size_of::<Sample>() as u64;
const CHECKPOINT_BYTES: u64 = size_of::<Checkpoint>() as u64;
/// A worker's two staging vectors.
const STAGING_BYTES: u64 =
    STAGING_SAMPLES as u64 * SAMPLE_BYTES + STAGING_CHECKPOINTS as u64 * CHECKPOINT_BYTES;
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
/// Fewest bytes between checkpoints, about 16 lines of a dump.
const MIN_INTERVAL: Bytes = Bytes(1 << 10);

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
    /// Least bytes between checkpoints, at least one; raised so the
    /// file's blocks fit `checkpoint_cap`.
    pub checkpoint_interval: Bytes,
    /// Most checkpoints kept, at least one.
    pub checkpoint_cap: usize,
    /// Bytes per chunk, rounded up to whole checkpoint blocks.
    pub chunk: Bytes,
    /// Read buffer per worker, at least one byte.
    pub buffer: Bytes,
    /// Workers whose buffers are reserved, at most one per chunk; at most
    /// one per core of them start.
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
    /// built. The pool is cut into [`MAX_THREADS`] shares, each a read
    /// buffer of 64 to 256 KiB and two staging vectors, and `threads` is
    /// how many of those fit it, so neither depends on the machine.
    /// [`scan`] reserves buffers for
    /// that many workers and starts at most one per core: one thread hashes
    /// short lines at about 1 GB/s, under a third of an SSD, so the scan is
    /// CPU-bound until several cores share it.
    #[must_use]
    pub fn for_pair(len_a: Bytes, len_b: Bytes, budget: &Budget) -> (Self, Self) {
        let half = budget.capacity(Half::Skeleton).0;
        let pool = half / 4;
        let most = MAX_THREADS as u64;
        let buffer = (pool / most)
            .saturating_sub(STAGING_BYTES)
            .clamp(MIN_BUFFER, MAX_BUFFER);
        let threads = (pool / (buffer + STAGING_BYTES)).clamp(1, most);
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
            // Blocks fill their cap whatever the lines, so they take half
            // the share. On dumps and CSV (lines under 128 bytes) that is
            // no more than a line index sized for 64-byte lines held, and
            // the map keeps the room it was sized with.
            let index = MIN_CHECKPOINTS + share(spare_checkpoints);
            let checkpoint_cap = index / 2;
            Self {
                level,
                sample_cap: to_usize(MIN_SAMPLES + share(spare_samples)),
                checkpoint_interval: MIN_INTERVAL,
                checkpoint_cap: to_usize(checkpoint_cap),
                chunk: Bytes(CHUNK),
                buffer: Bytes(buffer),
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
/// most one read buffer per scan thread in the first pass, one comparison
/// buffer per lane in the second. Holds no heap.
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
    /// Where the chunk hashes go, when a result is being saved.
    chunks: Option<ChunkTable<'w>>,
}

impl<'w> Meter<'w> {
    pub(crate) fn new(watch: &'w Watch, base: u64) -> Self {
        Self {
            watch,
            base,
            local: AtomicU64::new(0),
            chunks: None,
        }
    }

    /// [`new`](Self::new) for a scan that also fills `table`.
    pub(crate) fn hashing(watch: &'w Watch, base: u64, table: Option<ChunkTable<'w>>) -> Self {
        Self {
            chunks: table,
            ..Self::new(watch, base)
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
    /// A scan result built elsewhere, from the samples of stratum `level`
    /// in line order and the index, with `reservation` paying for both.
    pub(crate) fn from_parts(
        level: u32,
        samples: Vec<Sample>,
        index: LineIndex,
        reservation: Reservation,
    ) -> Self {
        Self {
            level,
            samples,
            index,
            reservation,
        }
    }

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

/// Bytes between checkpoints in the index a scan of `len` bytes with
/// `cfg` builds.
pub(crate) fn checkpoint_interval(len: u64, cfg: &ScanConfig) -> u64 {
    cfg.checkpoint_interval
        .0
        .max(len.div_ceil(cfg.checkpoint_cap as u64))
}

/// Skeleton-half bytes a scan of `len` bytes with `cfg` keeps at most
/// once it ends: the sample vector at its cap and one checkpoint per block.
pub(crate) fn kept_bytes(len: u64, cfg: &ScanConfig) -> u64 {
    cfg.sample_cap as u64 * SAMPLE_BYTES
        + len.div_ceil(checkpoint_interval(len, cfg)) * CHECKPOINT_BYTES
}

/// Skeleton-half bytes a scan of `len` bytes with `cfg` holds at most
/// while it runs: what it keeps, the workers' buffers and the chunk
/// counters. A half with this much free never refuses the scan, so its
/// stratum depends on the file alone.
pub(crate) fn peak_bytes(len: u64, cfg: &ScanConfig) -> u64 {
    let interval = checkpoint_interval(len, cfg);
    let chunk = cfg.chunk.0.div_ceil(interval).saturating_mul(interval);
    let chunks = len.div_ceil(chunk);
    let threads = cfg.threads.clamp(1, to_usize(chunks.max(1))) as u64;
    let buf_len = cfg.buffer.0.min(len + 1);
    kept_bytes(len, cfg) + threads * (buf_len + STAGING_BYTES) + chunks * 8
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
/// Panics if `cfg.chunk`, `cfg.buffer`, `cfg.checkpoint_interval` or
/// `cfg.checkpoint_cap` is zero.
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
    assert!(cfg.buffer.0 > 0, "buffer size must be positive");
    assert!(
        cfg.checkpoint_interval.0 > 0 && cfg.checkpoint_cap > 0,
        "checkpoint interval and cap must be positive"
    );
    let len = source.len().0;
    // Start at what the file is expected to produce; grow toward the cap.
    let lines = estimated_lines(Bytes(len));
    let sample_start = (lines >> cfg.level.min(63))
        .saturating_add(1)
        .min(cfg.sample_cap as u64);
    let interval = checkpoint_interval(len, cfg);
    let blocks = len.div_ceil(interval);
    let chunk = cfg.chunk.0.div_ceil(interval).saturating_mul(interval);
    let chunks = len.div_ceil(chunk);
    let threads = cfg.threads.clamp(1, to_usize(chunks.max(1)));
    // A chunk is read from one byte before its start, so a buffer of
    // `len + 1` holds any chunk of the file whole and never fills.
    let buf_len = cfg.buffer.0.min(len + 1);
    let keep = budget.try_reserve(
        Half::Skeleton,
        Bytes(sample_start * SAMPLE_BYTES + blocks * CHECKPOINT_BYTES),
    )?;
    let work = budget.try_reserve(
        Half::Skeleton,
        Bytes(threads as u64 * (buf_len + STAGING_BYTES) + chunks * 8),
    )?;

    let shared = Shared {
        samples: Mutex::new(Vec::with_capacity(to_usize(sample_start))),
        sample_cap: cfg.sample_cap,
        level: AtomicU32::new(cfg.level),
        checkpoints: Mutex::new(Vec::with_capacity(to_usize(blocks))),
        interval,
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
        let workers: Vec<_> = (0..threads.min(crate::budget::cores()))
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

    let mut checkpoints = shared
        .checkpoints
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    for c in &mut checkpoints {
        c.line += base(c.offset);
    }
    checkpoints.sort_unstable_by_key(|c| c.line);

    release_slack(&mut samples, &mut keep);
    release_slack(&mut checkpoints, &mut keep);

    Ok(Scanned {
        level,
        samples,
        index: LineIndex::new(checkpoints, total, Bytes(interval)),
        reservation: keep,
    })
}

/// State the workers share. Lines in `samples` and `checkpoints` are
/// chunk-local until the scan ends.
struct Shared<'m> {
    samples: Mutex<Vec<Sample>>,
    /// Most samples the vector may grow to before the stratum is raised.
    sample_cap: usize,
    level: AtomicU32,
    /// Sized for one checkpoint per block, so it never grows.
    checkpoints: Mutex<Vec<Checkpoint>>,
    /// Bytes per checkpoint block; chunks hold whole blocks.
    interval: u64,
    /// Bytes held for both vectors. Locked only while the sample vector's
    /// lock is held, always in that order, so the two cannot deadlock.
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

    /// Appends `staging` to the checkpoints, which have room for every
    /// block of the file.
    fn push_checkpoints(&self, staging: &mut Vec<Checkpoint>) {
        let mut out = self
            .checkpoints
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        debug_assert!(out.len() + staging.len() <= out.capacity());
        out.append(staging);
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

/// Even the coarsest stratum needs more samples than the configuration
/// allotted.
#[cold]
fn exceeded<T>(needed: usize, out: &[T]) -> Error {
    let bytes = |n: usize| Bytes((n * size_of::<T>()) as u64);
    Error::Budget(BudgetExceeded {
        half: Half::Skeleton,
        requested: bytes(needed),
        available: bytes(out.len()),
    })
}

/// One thread's view of the scan: its buffers, its copy of the stratum
/// and where the checkpoint block after its last checkpoint starts.
struct Worker<'a> {
    shared: &'a Shared<'a>,
    mask: u64,
    next_block: u64,
    samples: Vec<Sample>,
    checkpoints: Vec<Checkpoint>,
    /// Streaming hash of a line longer than the buffer, between reads.
    long: Option<Xxh3>,
    /// Hash of the current hash chunk, when a result is being saved.
    chunk_hash: Xxh3,
    /// The next byte to hash, and the end of what this chunk hashes.
    hash_at: u64,
    hash_end: u64,
}

impl<'a> Worker<'a> {
    fn new(shared: &'a Shared<'a>) -> Self {
        Self {
            shared,
            mask: strata::mask(shared.level.load(Ordering::Relaxed)),
            next_block: 0,
            samples: Vec::with_capacity(STAGING_SAMPLES),
            checkpoints: Vec::with_capacity(STAGING_CHECKPOINTS),
            long: None,
            chunk_hash: Xxh3::new(),
            hash_at: 0,
            hash_end: 0,
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
            let scanned = self
                .scan_chunk(source, &mut buf, i)
                .and_then(|lines| self.hash_rest(source, &mut buf).map(|()| lines));
            match scanned {
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
        self.shared.push_checkpoints(&mut self.checkpoints);
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
        if offset >= self.next_block {
            self.next_block = (offset + 1).next_multiple_of(self.shared.interval);
            self.checkpoints.push(Checkpoint {
                line: LineNo(local),
                offset: ByteOffset(offset),
            });
            if self.checkpoints.len() == STAGING_CHECKPOINTS {
                self.shared.push_checkpoints(&mut self.checkpoints);
            }
        }
        Ok(())
    }

    /// Hashes every line that starts in chunk `i` and returns how many.
    ///
    /// Reading starts one byte before the chunk so that a `\n` right at
    /// the boundary is seen; bytes up to and including the first `\n`
    /// belong to the previous chunk and are skipped. If no `\n` lies in
    /// `[start - 1, end - 1)`, no line starts in the chunk. The chunk
    /// starts a checkpoint block, so its first line is its block's first.
    fn scan_chunk<S: Source + ?Sized>(
        &mut self,
        source: &S,
        buf: &mut [u8],
        i: u64,
    ) -> Result<u64> {
        let nominal_start = i * self.shared.chunk;
        let nominal_end = (nominal_start + self.shared.chunk).min(self.shared.len);
        let mut skipping = i > 0;
        self.next_block = 0;
        self.hash_range(nominal_start, nominal_end);
        // `pos` is the file offset of `buf[0]`; `line_start` that of the
        // current line, which may lie before `pos` while streaming.
        let mut pos = nominal_start.saturating_sub(1);
        let mut line_start = pos;
        let mut filled = 0usize;
        let mut lines = 0u64;
        loop {
            self.shared.meter.check()?;
            let at = pos + filled as u64;
            // Up to a tail past the chunk; beyond it only while the line
            // that crosses the chunk's end is still open.
            let room = match (nominal_end + TAIL).saturating_sub(at) {
                0 => buf.len(),
                want => buf
                    .len()
                    .min(filled.saturating_add(usize::try_from(want).unwrap_or(usize::MAX))),
            };
            let n = read_retrying(source, &mut buf[filled..room], ByteOffset(at))?;
            self.hash(at, &buf[filled..filled + n])?;
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

    /// Sets the hash chunks of the scan chunk `start..end`: those that
    /// start in it, whole. None when no result is being saved.
    fn hash_range(&mut self, start: u64, end: u64) {
        let Some(t) = self.shared.meter.chunks else {
            return;
        };
        let size = 1u64 << t.shift;
        self.hash_at = start.next_multiple_of(size);
        self.hash_end = if self.hash_at < end {
            end.next_multiple_of(size).min(self.shared.len)
        } else {
            self.hash_at
        };
        self.chunk_hash.reset();
    }

    /// Hashes the part of `bytes`, read at `at`, that this chunk hashes,
    /// writing each hash chunk's hash once it is complete. Reads within a
    /// chunk are contiguous, so `at` never lies past `hash_at`.
    fn hash(&mut self, at: u64, bytes: &[u8]) -> Result<()> {
        let end = at + bytes.len() as u64;
        if self.hash_at >= self.hash_end || end <= self.hash_at {
            return Ok(());
        }
        let t = self
            .shared
            .meter
            .chunks
            .expect("a hash range implies a table");
        debug_assert!(at <= self.hash_at, "reads are contiguous");
        let stop = end.min(self.hash_end);
        while self.hash_at < stop {
            let chunk_end = (self.hash_at + 1)
                .next_multiple_of(1 << t.shift)
                .min(self.shared.len);
            let piece = chunk_end.min(stop);
            let from = usize::try_from(self.hash_at - at).expect("inside the buffer");
            let to = usize::try_from(piece - at).expect("inside the buffer");
            self.chunk_hash.update(&bytes[from..to]);
            self.hash_at = piece;
            if piece == chunk_end {
                t.put((chunk_end - 1) >> t.shift, self.chunk_hash.digest())?;
                self.chunk_hash.reset();
            }
        }
        Ok(())
    }

    /// Reads and hashes what the scan of the chunk left of its hash
    /// chunks: the end of the last one, past the scan chunk's end.
    fn hash_rest<S: Source + ?Sized>(&mut self, source: &S, buf: &mut [u8]) -> Result<()> {
        while self.hash_at < self.hash_end {
            self.shared.meter.check()?;
            let at = self.hash_at;
            let want = usize::try_from(self.hash_end - at)
                .unwrap_or(usize::MAX)
                .min(buf.len());
            let n = read_retrying(source, &mut buf[..want], ByteOffset(at))?;
            if n == 0 {
                return Err(Error::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "source ended before its length",
                )));
            }
            self.hash(at, &buf[..n])?;
        }
        Ok(())
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
        assert!(a.buffer >= Bytes(MIN_BUFFER) && a.buffer <= Bytes(MAX_BUFFER));
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
        let buffers = a.threads as u64 * (a.buffer.0 + STAGING_BYTES);
        let floors = 2 * (MIN_SAMPLES * SAMPLE_BYTES + MIN_CHECKPOINTS * CHECKPOINT_BYTES);
        if half >= floors + buffers {
            assert!(kept(a) + kept(b) + buffers <= half, "{half} {a:?} {b:?}");
        }
        assert!(kept(a) + kept(b) + table(a) + table(b) <= half.max(floors * 2));
    }
}
