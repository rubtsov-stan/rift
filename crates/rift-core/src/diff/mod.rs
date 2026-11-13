//! The whole pipeline: skeleton, then refinement on demand, as a
//! [`DiffView`] and as a unified diff.
//!
//! [`Diff::new`] scans both sources, builds the segment map and starts a
//! [`Scheduler`] whose job refines one segment: it hashes the segment's
//! lines and runs the [`Refiner`] on them. A segment whose estimate exceeds
//! the most one job may reserve is resampled at a lower stratum inside the
//! segment, and its gaps are refined or resampled again; only a range that
//! still does not fit, and is not equal, becomes a whole replacement marked
//! approximate (see `resample`).
//!
//! Memory. Skeleton half: the two line indexes and the segment list, kept
//! for the life of the diff; the samples are released once the map is
//! built. Refine half, in this order: the front end's share (two line
//! readers, the line buffer and the 1 MiB output buffer of the unified
//! writer), then the moved blocks' list, taken with the search off too,
//! then two line readers and four line buffers for each worker the budget
//! allows, whether it starts or not, then, while the blocks are found, two
//! comparison buffers (see [`Moves`]), then the scheduler, which takes what
//! is left for its row table, its hunk cache and the jobs' working sets. Line
//! buffers are bounded because a reader returns a line whole only when it
//! fits its buffer; longer lines travel in pieces of 64 KiB.

mod resample;
#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::io::Write;
use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError};

use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use crate::budget::{Budget, BudgetExceeded, Half, MAX_THREADS, Reservation};
use crate::edit::{ApproxReason, Edit, Hunk};
use crate::error::{Error, Result};
use crate::index::{IndexedLines, LineIndex};
use crate::lines::{LineAccess, LineBuf};
use crate::moves::{MovedBlock, Moves};
use crate::refine::{Refiner, VERIFY_LINES, work_estimate};
use crate::scan::Watch;
use crate::sched::{Cancel, HUNK_OVERHEAD, Job, Scheduler, least_bytes};
use crate::script::{LINES_PER_READ, PIECE, for_each_piece};
use crate::segment::{Segment, SegmentStatus};
use crate::skeleton::{OffChain, build_pair_watched};
use crate::source::Source;
use crate::types::{ByteOffset, Bytes, LineHash, LineNo};
use crate::unified::{Options, Verdict, write_diff};
use crate::view::{ApproxTally, DiffView, HunkState, Progress};

/// Read buffer of each worker's line readers. Lines longer than this are
/// hashed and compared in pieces.
const WORKER_SCRATCH: Bytes = Bytes(4 << 10);
/// Read buffer of the front end's line readers.
const FRONT_SCRATCH: Bytes = Bytes(16 << 10);
/// Output buffer of the unified writer.
const WRITER_BUF: Bytes = Bytes(1 << 20);
/// Myers cost cutoff per gap, the floor `git` uses.
const CUTOFF: u64 = 256;
/// Segments ahead of the unified writer that are refined in advance, per
/// worker.
const AHEAD_PER_WORKER: usize = 8;
const EDIT_BYTES: u64 = size_of::<Edit>() as u64;

/// Most bytes a line buffer reaches when filled through a reader with a
/// `scratch`-byte buffer: a block of whole lines, each shorter than the
/// buffer, or one piece of a longer line, doubled for `Vec` growth.
fn line_buf_bytes(scratch: Bytes) -> u64 {
    let lines = LINES_PER_READ.max(VERIFY_LINES as u64);
    2 * ((lines * scratch.0).max(PIECE.0) + lines * size_of::<usize>() as u64)
}

/// Heap one refinement of `n` and `m` lines takes: the line hashes, the
/// refiner's workspace and the hunk at one edit per line.
fn estimate(n: u64, m: u64) -> Bytes {
    let hashes = 8 * (n + m);
    let hunk = HUNK_OVERHEAD.0 + EDIT_BYTES * (n + m);
    Bytes(hashes + work_estimate(n, m).0 + hunk)
}

/// Settings of a [`Diff`] other than the sources, the budget and the
/// thread count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    /// Look for moved blocks. When false no seed is compared, but the
    /// block list's share is set aside all the same, so the edit script
    /// and its approximate marks are the same either way.
    pub moves: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { moves: true }
    }
}

/// A diff of two sources, refined lazily under a memory budget.
///
/// Implements [`DiffView`] for the terminal interface; [`write_unified`]
/// writes the whole diff. Dropping it stops the workers and releases its
/// share of the budget.
///
/// [`write_unified`]: Diff::write_unified
#[derive(Debug)]
pub struct Diff<A: Source + Send + ?Sized + 'static, B: Source + Send + ?Sized + 'static> {
    sched: Scheduler<RefineJob<A, B>>,
    /// Where each segment starts in both files; in the skeleton half.
    starts: Arc<[[ByteOffset; 2]]>,
    lines_a: Reader<A>,
    lines_b: Reader<B>,
    threads: usize,
    level: u32,
    anchors: usize,
    /// In the skeleton half, with its own reservation.
    off_chain: OffChain,
    /// In the refine half, taken before the scheduler takes the rest.
    moves: Moves,
    _map: [Reservation; 3],
    _front: Reservation,
}

type Reader<S> = IndexedLines<Arc<S>, Arc<LineIndex>>;

/// What [`Diff::write_unified`] wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Written {
    /// Whether the files differ.
    pub verdict: Verdict,
    /// Segments whose hunk is approximate.
    pub approx_segments: u64,
    /// Lines of A plus lines of B in those segments.
    pub approx_lines: u64,
    /// Approximate segments by reason, indexed like [`ApproxReason::ALL`].
    pub approx_reasons: [u64; ApproxReason::ALL.len()],
}

impl<A: Source + Send + ?Sized + 'static, B: Source + Send + ?Sized + 'static> Diff<A, B> {
    /// Scans `a` and `b`, builds the segment map and starts up to `threads`
    /// refinement workers. The Refine half holds buffers for as many
    /// workers as fit half of what it has free, at most [`MAX_THREADS`],
    /// whatever `threads` is, so the diff does not depend on it; fewer
    /// workers start when `threads` is smaller.
    ///
    /// # Errors
    ///
    /// [`Error::Budget`] when the budget cannot hold the map, the front
    /// end's share and one worker; [`Error::Io`] from either source.
    pub fn new(a: Arc<A>, b: Arc<B>, budget: &Budget, threads: usize) -> Result<Self> {
        Self::with_config(a, b, budget, threads, Config::default())
    }

    /// [`Diff::new`] with settings other than the defaults.
    ///
    /// # Errors
    ///
    /// As for [`Diff::new`].
    pub fn with_config(
        a: Arc<A>,
        b: Arc<B>,
        budget: &Budget,
        threads: usize,
        config: Config,
    ) -> Result<Self> {
        Self::with_watch(a, b, budget, threads, config, &Watch::new())
    }

    /// [`Diff::with_config`] whose scan is followed and stopped through
    /// `watch`, as [`build_pair_watched`] describes. The watch is read once
    /// more before the move search, which reads both files; after that the
    /// build cannot be stopped, and cancelling the watch once the map is
    /// built does not stop refinement.
    ///
    /// # Errors
    ///
    /// As for [`Diff::new`], and [`Error::Cancelled`] once `watch` is
    /// cancelled during the scan.
    pub fn with_watch(
        a: Arc<A>,
        b: Arc<B>,
        budget: &Budget,
        threads: usize,
        config: Config,
        watch: &Watch,
    ) -> Result<Self> {
        if watch.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let (scan_a, scan_b, skeleton) = build_pair_watched(&*a, &*b, budget, watch)?;
        let (level, anchors) = (skeleton.level(), skeleton.anchors());
        let (index_a, held_a) = scan_a.into_index();
        let (index_b, held_b) = scan_b.into_index();
        let (segments, starts, held_map, off_chain) = skeleton.into_parts();
        let starts: Arc<[[ByteOffset; 2]]> = starts.into();
        let (index_a, index_b) = (Arc::new(index_a), Arc::new(index_b));

        // Each worker: two readers and four line buffers (two in the
        // refiner, two for hashing and comparing). Keep at least half of
        // what is left, after the front end's two readers, for the
        // scheduler.
        let per_worker = 2 * WORKER_SCRATCH.0 + 4 * line_buf_bytes(WORKER_SCRATCH);
        let front_bytes = WRITER_BUF.0 + line_buf_bytes(FRONT_SCRATCH);
        // What one worker needs in all, refused at once rather than one
        // reservation at a time, so the error names the whole amount. The
        // readers of the move search are released before the front end's
        // larger ones are taken, so they add nothing to the peak.
        let least = front_bytes + per_worker + 2 * FRONT_SCRATCH.0 + least_bytes(segments.len()).0;
        let free = budget.available(Half::Refine);
        if free.0 < least {
            return Err(BudgetExceeded {
                half: Half::Refine,
                requested: Bytes(least),
                available: free,
            }
            .into());
        }
        let front = budget.try_reserve(Half::Refine, Bytes(front_bytes))?;
        let list = move_list(budget, off_chain.pairs().len(), least - front_bytes);
        let free = budget
            .available(Half::Refine)
            .0
            .saturating_sub(2 * FRONT_SCRATCH.0);
        // The allowance follows the budget alone, so the scheduler's share
        // does too; `threads`, the machine's cores, only decides how many of
        // the allowed workers start. The readers of the workers that start
        // are reserved with them, the rest of the allowance here.
        let allowance = usize::try_from(free / 2 / per_worker)
            .unwrap_or(MAX_THREADS)
            .clamp(1, MAX_THREADS);
        let threads = threads.clamp(1, allowance);
        let pool = budget.try_reserve(
            Half::Refine,
            Bytes(allowance as u64 * per_worker - threads as u64 * 2 * WORKER_SCRATCH.0),
        )?;
        let workers = (0..threads)
            .map(|_| {
                Ok(Worker {
                    refiner: Refiner::new(CUTOFF),
                    a: IndexedLines::new(
                        Arc::clone(&a),
                        Arc::clone(&index_a),
                        budget,
                        Half::Refine,
                        WORKER_SCRATCH,
                    )?,
                    b: IndexedLines::new(
                        Arc::clone(&b),
                        Arc::clone(&index_b),
                        budget,
                        Half::Refine,
                        WORKER_SCRATCH,
                    )?,
                    buf: LineBuf::new(),
                    buf2: LineBuf::new(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        // Readers of their own with small buffers keep the comparison
        // buffers small; all of it is released once the blocks are found.
        let moves = if config.moves {
            if watch.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let (ix_a, ix_b) = (&*index_a, &*index_b);
            let ma = IndexedLines::new(&*a, ix_a, budget, Half::Refine, WORKER_SCRATCH)?;
            let mb = IndexedLines::new(&*b, ix_b, budget, Half::Refine, WORKER_SCRATCH)?;
            Moves::find(
                off_chain.pairs(),
                &segments,
                &ma,
                &mb,
                WORKER_SCRATCH,
                budget,
                list,
            )?
        } else {
            Moves::off_holding(list)
        };
        let lines_a = IndexedLines::new(a, index_a, budget, Half::Refine, FRONT_SCRATCH)?;
        let lines_b = IndexedLines::new(b, index_b, budget, Half::Refine, FRONT_SCRATCH)?;
        let job = RefineJob {
            workers: Mutex::new(workers),
            starts: Arc::clone(&starts),
            _pool: pool,
        };
        let sched = Scheduler::new(job, segments, budget, threads)?;
        Ok(Self {
            sched,
            starts,
            lines_a,
            lines_b,
            threads,
            level,
            anchors,
            off_chain,
            moves,
            _map: [held_a, held_b, held_map],
            _front: front,
        })
    }

    /// Lines of A, for a front end.
    #[must_use]
    pub fn lines_a(&self) -> &(impl LineAccess + use<A, B>) {
        &self.lines_a
    }

    /// Lines of B, for a front end.
    #[must_use]
    pub fn lines_b(&self) -> &(impl LineAccess + use<A, B>) {
        &self.lines_b
    }

    /// The stratum the skeleton's anchors were drawn from.
    #[must_use]
    pub fn level(&self) -> u32 {
        self.level
    }

    /// Anchors in the skeleton.
    #[must_use]
    pub fn anchors(&self) -> usize {
        self.anchors
    }

    /// Anchor candidates the skeleton's chain left out.
    #[must_use]
    pub fn off_chain(&self) -> &OffChain {
        &self.off_chain
    }

    /// Moved blocks, or the note that the budget did not allow looking.
    #[must_use]
    pub fn moves(&self) -> &Moves {
        &self.moves
    }

    /// Refinement workers.
    #[must_use]
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// [`DiffView::hunk`] that returns a failed refinement as its error,
    /// typed as the job returned it, instead of panicking.
    ///
    /// # Errors
    ///
    /// The refinement's error: [`Error::Budget`] when the segment could
    /// not be refined within the budget, [`Error::Io`] from either source.
    ///
    /// # Panics
    ///
    /// Panics if `segment` is out of range.
    pub fn try_hunk(&self, segment: usize) -> Result<HunkState> {
        self.sched.try_hunk(segment)
    }

    /// Whether `segment` has been refined and, if so, whether the result is
    /// approximate. Unlike [`try_hunk`](Self::try_hunk) it queues nothing:
    /// `None` for a segment not refined yet.
    ///
    /// # Panics
    ///
    /// Panics if `segment` is out of range.
    #[must_use]
    pub fn refined_approx(&self, segment: usize) -> Option<bool> {
        self.sched.refined_approx(segment)
    }

    /// Writes the diff as `diff -u` would, walking segments in order
    /// through the scheduler. Background refinement is switched off and the
    /// visible window moved ahead of the writer instead, so the workers
    /// refine the next few segments while earlier ones are written.
    ///
    /// # Errors
    ///
    /// As for [`write_diff`], including a failed refinement.
    pub fn write_unified(&self, opts: &Options<'_>, out: impl Write) -> Result<Written> {
        let segs = self.sched.segments();
        let n = segs.len();
        self.sched.set_background(false);
        let ahead = AHEAD_PER_WORKER * self.threads;
        let front = Cell::new(None::<usize>);
        let window = Cell::new(None::<usize>);
        let approx = Cell::new((0u64, 0u64));
        let reasons = Cell::new([0u64; ApproxReason::ALL.len()]);
        let hunk = |i: usize| -> Result<Option<Arc<Hunk>>> {
            if i >= n {
                return Ok(None);
            }
            let first_visit = front.get().is_none_or(|f| i > f);
            if first_visit {
                front.set(Some(i));
            }
            // The window moves a quarter of its length at a time: each move
            // takes the queue lock the workers share.
            if window.get().is_none_or(|w| i >= w + ahead / 4) {
                window.set(Some(i));
                let last = &segs[(i + ahead).min(n - 1)];
                let start = segs[i].a.start.0.saturating_sub(1);
                self.sched
                    .set_visible(LineNo(start)..LineNo(last.a.end.0.max(start + 1)));
            }
            let h = self.sched.wait_hunk(i)?;
            if first_visit && let Some(reason) = h.approx {
                let (s, l) = approx.get();
                approx.set((s + 1, l + segs[i].len_a() + segs[i].len_b()));
                let mut by_reason = reasons.get();
                by_reason[reason as usize] += 1;
                reasons.set(by_reason);
            }
            Ok(Some(h))
        };
        let lines_a = Sought {
            lines: &self.lines_a,
            segs,
            starts: &self.starts,
            side: 0,
        };
        let lines_b = Sought {
            lines: &self.lines_b,
            segs,
            starts: &self.starts,
            side: 1,
        };
        let verdict = write_diff(&lines_a, &lines_b, hunk, opts, out)?;
        let (approx_segments, approx_lines) = approx.get();
        Ok(Written {
            verdict,
            approx_segments,
            approx_lines,
            approx_reasons: reasons.get(),
        })
    }
}

impl<A: Source + Send + ?Sized + 'static, B: Source + Send + ?Sized + 'static> DiffView
    for Diff<A, B>
{
    fn len_a(&self) -> u64 {
        self.lines_a.line_count()
    }

    fn len_b(&self) -> u64 {
        self.lines_b.line_count()
    }

    fn segments(&self) -> &[Segment] {
        self.sched.segments()
    }

    fn hunk(&self, segment: usize) -> HunkState {
        self.sched.hunk(segment)
    }

    fn progress(&self) -> Progress {
        self.sched.progress()
    }

    fn approx_refined(&self) -> ApproxTally {
        self.sched.approx_refined()
    }

    fn set_visible(&self, range: Range<LineNo>) {
        self.sched.set_visible(range);
    }

    fn moved_blocks(&self) -> &[MovedBlock] {
        self.moves.blocks()
    }

    fn moves_disabled(&self) -> bool {
        self.moves.disabled()
    }
}

/// The moved blocks' list for `pairs` seeds, when the Refine half keeps
/// `rest` free besides it. Sized from the budget and the seeds and taken
/// with the search off too, before the workers, so neither the search nor
/// the cores change what the scheduler gets.
fn move_list(budget: &Budget, pairs: usize, rest: u64) -> Option<Reservation> {
    let list = Moves::list_bytes(pairs, budget);
    (budget.available(Half::Refine).0 >= rest + list.0)
        .then(|| budget.try_reserve(Half::Refine, list).ok())
        .flatten()
}

/// A front-end reader that, before each read, points the reader at the
/// start of the segment holding the first line asked for, or of the next
/// segment when that starts a few lines later. The unified writer reads
/// near segment starts, and without this every read would scan from the
/// checkpoint before it, mostly through bytes no worker has read since the
/// skeleton was built, so they come from the disk.
struct Sought<'d, S: Source + ?Sized> {
    lines: &'d Reader<S>,
    segs: &'d [Segment],
    starts: &'d [[ByteOffset; 2]],
    /// 0 for A, 1 for B.
    side: usize,
}

impl<S: Source + Send + ?Sized> Sought<'_, S> {
    fn aim(&self, line: LineNo) {
        let first = |s: &Segment| if self.side == 0 { s.a.start } else { s.b.start };
        let next = self.segs.partition_point(|s| first(s) <= line);
        let k = match self.segs.get(next) {
            Some(s) if first(s) - line <= AIM_BACK => next,
            _ if next > 0 => next - 1,
            _ => return,
        };
        self.lines
            .seek(first(&self.segs[k]), self.starts[k][self.side]);
    }
}

/// How far before a segment's start a read still aims at it: the context
/// lines the writer prints before a change.
const AIM_BACK: u64 = 8;

impl<S: Source + Send + ?Sized> LineAccess for Sought<'_, S> {
    fn line_count(&self) -> u64 {
        self.lines.line_count()
    }

    fn read_lines(&self, range: Range<LineNo>, buf: &mut LineBuf) -> Result<()> {
        if !range.is_empty() {
            self.aim(range.start);
        }
        self.lines.read_lines(range, buf)
    }

    fn read_line_prefix(&self, line: LineNo, max: Bytes, buf: &mut LineBuf) -> Result<Bytes> {
        self.aim(line);
        self.lines.read_line_prefix(line, max, buf)
    }

    fn read_line_part(
        &self,
        line: LineNo,
        from: Bytes,
        max: Bytes,
        buf: &mut LineBuf,
    ) -> Result<bool> {
        self.aim(line);
        self.lines.read_line_part(line, from, max, buf)
    }
}

/// Per-thread state of the refinement job.
#[derive(Debug)]
struct Worker<A: ?Sized, B: ?Sized> {
    refiner: Refiner,
    a: Reader<A>,
    b: Reader<B>,
    buf: LineBuf,
    buf2: LineBuf,
}

/// Refines segments with one [`Worker`] per scheduler thread.
#[derive(Debug)]
struct RefineJob<A: ?Sized, B: ?Sized> {
    workers: Mutex<Vec<Worker<A, B>>>,
    /// Where each segment starts in both files, so a worker reads it
    /// without scanning from a checkpoint; in the skeleton half.
    starts: Arc<[[ByteOffset; 2]]>,
    /// The workers' line buffers, which grow on use.
    _pool: Reservation,
}

/// A worker taken from the pool, returned on drop, also when a job panics.
struct Lease<'p, W> {
    pool: &'p Mutex<Vec<W>>,
    worker: Option<W>,
}

impl<W> Drop for Lease<'_, W> {
    fn drop(&mut self) {
        if let Some(w) = self.worker.take() {
            self.pool
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(w);
        }
    }
}

impl<A: Source + Send + ?Sized + 'static, B: Source + Send + ?Sized + 'static> RefineJob<A, B> {
    fn lease(&self) -> Lease<'_, Worker<A, B>> {
        let worker = self
            .workers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        Lease {
            pool: &self.workers,
            worker: Some(worker.expect("one worker per scheduler thread")),
        }
    }
}

impl<A: Source + Send + ?Sized + 'static, B: Source + Send + ?Sized + 'static> Job
    for RefineJob<A, B>
{
    fn estimate(&self, segment: &Segment) -> Bytes {
        estimate(segment.len_a(), segment.len_b())
    }

    fn run(
        &self,
        index: usize,
        segment: &Segment,
        reservation: &mut Reservation,
        cancel: &Cancel,
    ) -> Result<Hunk> {
        let mut lease = self.lease();
        let w = lease.worker.as_mut().expect("leased");
        let start = Some((segment, self.starts[index]));
        refine(
            w,
            segment.a.clone(),
            segment.b.clone(),
            start,
            reservation,
            cancel,
        )
    }

    fn too_large(
        &self,
        index: usize,
        segment: &Segment,
        reservation: &mut Reservation,
        cancel: &Cancel,
    ) -> Result<Hunk> {
        let mut lease = self.lease();
        let w = lease.worker.as_mut().expect("leased");
        seek(w, segment, self.starts[index]);
        resample::refine_large(w, segment, reservation.bytes(), cancel)
    }
}

/// Points both readers of `w` at the start of `segment`.
fn seek<A: Source + Send + ?Sized, B: Source + Send + ?Sized>(
    w: &Worker<A, B>,
    segment: &Segment,
    start: [ByteOffset; 2],
) {
    w.a.seek(segment.a.start, start[0]);
    w.b.seek(segment.b.start, start[1]);
}

/// Refines A lines `ra` against B lines `rb` within `reservation`, which
/// should hold [`estimate`] of their lengths. `start` gives the byte
/// offsets of a segment that begins there, when known.
fn refine<A: Source + Send + ?Sized, B: Source + Send + ?Sized>(
    w: &mut Worker<A, B>,
    ra: Range<LineNo>,
    rb: Range<LineNo>,
    start: Option<(&Segment, [ByteOffset; 2])>,
    reservation: &mut Reservation,
    cancel: &Cancel,
) -> Result<Hunk> {
    // Hashing reads the segment forward and leaves the readers at its end;
    // verification starts over from its first line.
    if let Some((seg, at)) = start {
        seek(w, seg, at);
    }
    let ha = hashes(&w.a, ra.clone(), &mut w.buf)?;
    let hb = hashes(&w.b, rb.clone(), &mut w.buf)?;
    if cancel.is_set() {
        return Ok(Hunk::default());
    }
    if let Some((seg, at)) = start {
        seek(w, seg, at);
    }
    let seg = Segment {
        a: ra,
        b: rb,
        status: SegmentStatus::Differs,
    };
    w.refiner.refine(&seg, &ha, &hb, &w.a, &w.b, reservation)
}

/// Hashes of the lines `range`, exactly sized.
fn hashes(
    lines: &impl LineAccess,
    range: Range<LineNo>,
    buf: &mut LineBuf,
) -> Result<Vec<LineHash>> {
    let mut out = Vec::new();
    let n = usize::try_from(range.end - range.start).unwrap_or(usize::MAX);
    out.try_reserve_exact(n).map_err(|_| oom())?;
    each_hash(lines, range, buf, |h| out.push(h))?;
    Ok(out)
}

/// Calls `f` with the hash of every line of `range`, in order: xxh3 of the
/// whole line, terminator included, streamed for a line read in pieces.
fn each_hash(
    lines: &impl LineAccess,
    range: Range<LineNo>,
    buf: &mut LineBuf,
    mut f: impl FnMut(LineHash),
) -> Result<()> {
    let mut long = Xxh3::new();
    for_each_piece(lines, range, buf, |piece, first, last| {
        if first && last {
            f(xxh3_64(piece));
        } else {
            if first {
                long.reset();
            }
            long.update(piece);
            if last {
                f(long.digest());
            }
        }
        Ok(())
    })
}

fn oom() -> Error {
    std::io::Error::from(std::io::ErrorKind::OutOfMemory).into()
}

/// Whether A lines `ra` and B lines `rb`, the same count, are equal byte
/// by byte. Reads blocks of whole lines, and lines longer than the reader
/// pair by pair in pieces.
fn same_lines<A: Source + Send + ?Sized, B: Source + Send + ?Sized>(
    w: &mut Worker<A, B>,
    ra: Range<LineNo>,
    rb: Range<LineNo>,
) -> Result<bool> {
    debug_assert_eq!(ra.end - ra.start, rb.end - rb.start);
    let lines = ra.end - ra.start;
    let mut done = 0;
    while done < lines {
        let block = (lines - done).min(VERIFY_LINES as u64);
        let (at_a, at_b) = (ra.start + done, rb.start + done);
        let whole = whole_lines(&w.a, at_a..at_a + block, &mut w.buf)?
            && whole_lines(&w.b, at_b..at_b + block, &mut w.buf2)?;
        if whole {
            if w.buf.iter().zip(w.buf2.iter()).any(|(x, y)| x != y) {
                return Ok(false);
            }
        } else {
            for j in 0..block {
                let same = crate::refine::same_line(
                    &w.a,
                    at_a + j,
                    &w.b,
                    at_b + j,
                    &mut w.buf,
                    &mut w.buf2,
                )?;
                if !same {
                    return Ok(false);
                }
            }
        }
        done += block;
    }
    Ok(true)
}

/// Reads `range` into `buf`; false when a line is too long for the reader
/// and the block has to be compared pair by pair in pieces.
fn whole_lines(lines: &impl LineAccess, range: Range<LineNo>, buf: &mut LineBuf) -> Result<bool> {
    match lines.read_lines(range, buf) {
        Ok(()) => Ok(true),
        Err(Error::LineTooLong { .. }) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Edits of a large segment as they are produced, within a fixed byte
/// cap. Two slots are kept free so the rest of the segment can always be
/// closed with a whole replacement.
struct Out {
    edits: Vec<Edit>,
    cap: usize,
    approx: Option<ApproxReason>,
    /// End of what the edits cover so far.
    at_a: LineNo,
    at_b: LineNo,
    _reservation: Reservation,
}

impl Out {
    fn new(budget: &Budget, half: Half, bytes: Bytes, start: (LineNo, LineNo)) -> Result<Self> {
        let cap = usize::try_from(bytes.0.saturating_sub(HUNK_OVERHEAD.0) / EDIT_BYTES)
            .unwrap_or(usize::MAX);
        if cap < 2 {
            return Err(BudgetExceeded {
                half,
                requested: Bytes(HUNK_OVERHEAD.0 + 2 * EDIT_BYTES),
                available: bytes,
            }
            .into());
        }
        let reservation = budget.try_reserve(half, bytes)?;
        Ok(Self {
            edits: Vec::new(),
            cap,
            approx: None,
            at_a: start.0,
            at_b: start.1,
            _reservation: reservation,
        })
    }

    /// Appends `e`, merged into the last edit when they are contiguous runs
    /// of one kind. False when it would take one of the two spare slots.
    fn push(&mut self, e: Edit) -> Result<bool> {
        self.add(e, 2)
    }

    fn add(&mut self, e: Edit, spare: usize) -> Result<bool> {
        let merged = match (self.edits.last_mut(), e) {
            (Some(Edit::Equal { a, b, len }), Edit::Equal { a: x, b: y, len: k })
                if *a + *len == x && *b + *len == y =>
            {
                *len += k;
                true
            }
            (Some(Edit::Delete { a, len }), Edit::Delete { a: x, len: k }) if *a + *len == x => {
                *len += k;
                true
            }
            (Some(Edit::Insert { b, len }), Edit::Insert { b: y, len: k }) if *b + *len == y => {
                *len += k;
                true
            }
            _ => false,
        };
        if !merged {
            if self.edits.len() + 1 + spare > self.cap {
                return Ok(false);
            }
            if self.edits.len() == self.edits.capacity() {
                let want = (2 * self.edits.len()).max(16).min(self.cap) - self.edits.len();
                self.edits.try_reserve_exact(want).map_err(|_| oom())?;
            }
            self.edits.push(e);
        }
        match e {
            Edit::Equal { len, .. } => {
                self.at_a += len;
                self.at_b += len;
            }
            Edit::Delete { len, .. } => self.at_a += len,
            Edit::Insert { len, .. } => self.at_b += len,
        }
        Ok(true)
    }

    /// Replaces A lines `ra` by B lines `rb`, marking the hunk approximate
    /// for `reason`.
    fn replace(
        &mut self,
        ra: Range<LineNo>,
        rb: Range<LineNo>,
        spare: usize,
        reason: ApproxReason,
    ) -> Result<bool> {
        self.approx = self.approx.max(Some(reason));
        if !ra.is_empty()
            && !self.add(
                Edit::Delete {
                    a: ra.start,
                    len: ra.end - ra.start,
                },
                spare,
            )?
        {
            return Ok(false);
        }
        if !rb.is_empty()
            && !self.add(
                Edit::Insert {
                    b: rb.start,
                    len: rb.end - rb.start,
                },
                spare,
            )?
        {
            return Ok(false);
        }
        Ok(true)
    }

    fn into_hunk(self) -> Hunk {
        Hunk {
            edits: self.edits,
            approx: self.approx,
        }
    }
}
