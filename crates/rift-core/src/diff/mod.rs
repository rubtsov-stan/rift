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
//! writer), then each worker's two line readers and four line buffers,
//! then the scheduler, which takes what is left for its row table, its
//! hunk cache and the jobs' working sets. Line buffers are bounded because
//! a reader returns a line whole only when it fits its buffer; longer lines
//! travel in pieces of 64 KiB.

mod resample;
#[cfg(test)]
mod tests;

use std::cell::Cell;
use std::io::Write;
use std::ops::Range;
use std::sync::{Arc, Mutex, PoisonError};

use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use crate::budget::{Budget, BudgetExceeded, Half, Reservation};
use crate::edit::{Edit, Hunk};
use crate::error::{Error, Result};
use crate::index::{IndexedLines, LineIndex};
use crate::lines::{LineAccess, LineBuf};
use crate::refine::{Refiner, VERIFY_LINES, work_estimate};
use crate::sched::{Cancel, HUNK_OVERHEAD, Job, Scheduler};
use crate::script::{LINES_PER_READ, PIECE, for_each_piece};
use crate::segment::{Segment, SegmentStatus};
use crate::skeleton::build_pair;
use crate::source::Source;
use crate::types::{Bytes, LineHash, LineNo};
use crate::unified::{Options, Verdict, write_diff};
use crate::view::{DiffView, HunkState, Progress};

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
    lines_a: Reader<A>,
    lines_b: Reader<B>,
    threads: usize,
    level: u32,
    anchors: usize,
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
}

impl<A: Source + Send + ?Sized + 'static, B: Source + Send + ?Sized + 'static> Diff<A, B> {
    /// Scans `a` and `b`, builds the segment map and starts up to `threads`
    /// refinement workers, fewer when the Refine half cannot hold their
    /// buffers.
    ///
    /// # Errors
    ///
    /// [`Error::Budget`] when the budget cannot hold the map, the front
    /// end's share and one worker; [`Error::Io`] from either source.
    pub fn new(a: Arc<A>, b: Arc<B>, budget: &Budget, threads: usize) -> Result<Self> {
        let (scan_a, scan_b, skeleton) = build_pair(&*a, &*b, budget)?;
        let (level, anchors) = (skeleton.level(), skeleton.anchors());
        let (index_a, held_a) = scan_a.into_index();
        let (index_b, held_b) = scan_b.into_index();
        let (segments, held_map) = skeleton.into_parts();
        let (index_a, index_b) = (Arc::new(index_a), Arc::new(index_b));

        let front = budget.try_reserve(
            Half::Refine,
            Bytes(WRITER_BUF.0 + line_buf_bytes(FRONT_SCRATCH)),
        )?;

        // Each worker: two readers and four line buffers (two in the
        // refiner, two for hashing and comparing). Keep at least half of
        // what is left, after the front end's two readers, for the
        // scheduler.
        let per_worker = 2 * WORKER_SCRATCH.0 + 4 * line_buf_bytes(WORKER_SCRATCH);
        let free = budget
            .available(Half::Refine)
            .0
            .saturating_sub(2 * FRONT_SCRATCH.0);
        let threads = threads.clamp(
            1,
            usize::try_from(free / 2 / per_worker).unwrap_or(1).max(1),
        );
        let pool = budget.try_reserve(
            Half::Refine,
            Bytes(threads as u64 * (per_worker - 2 * WORKER_SCRATCH.0)),
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
        let lines_a = IndexedLines::new(a, index_a, budget, Half::Refine, FRONT_SCRATCH)?;
        let lines_b = IndexedLines::new(b, index_b, budget, Half::Refine, FRONT_SCRATCH)?;
        let job = RefineJob {
            workers: Mutex::new(workers),
            _pool: pool,
        };
        let sched = Scheduler::new(job, segments, budget, threads)?;
        Ok(Self {
            sched,
            lines_a,
            lines_b,
            threads,
            level,
            anchors,
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

    /// Refinement workers.
    #[must_use]
    pub fn threads(&self) -> usize {
        self.threads
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
        let approx = Cell::new((0u64, 0u64));
        let hunk = |i: usize| -> Result<Option<Arc<Hunk>>> {
            if i >= n {
                return Ok(None);
            }
            let first_visit = front.get().is_none_or(|f| i > f);
            if first_visit {
                front.set(Some(i));
                let last = &segs[(i + ahead).min(n - 1)];
                let start = segs[i].a.start.0.saturating_sub(1);
                self.sched
                    .set_visible(LineNo(start)..LineNo(last.a.end.0.max(start + 1)));
            }
            let h = self.sched.wait_hunk(i)?;
            if first_visit && h.approx {
                let (s, l) = approx.get();
                approx.set((s + 1, l + segs[i].len_a() + segs[i].len_b()));
            }
            Ok(Some(h))
        };
        let verdict = write_diff(&self.lines_a, &self.lines_b, hunk, opts, out)?;
        let (approx_segments, approx_lines) = approx.get();
        Ok(Written {
            verdict,
            approx_segments,
            approx_lines,
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

    fn set_visible(&self, range: Range<LineNo>) {
        self.sched.set_visible(range);
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
        segment: &Segment,
        reservation: &mut Reservation,
        cancel: &Cancel,
    ) -> Result<Hunk> {
        let mut lease = self.lease();
        let w = lease.worker.as_mut().expect("leased");
        refine(w, segment.a.clone(), segment.b.clone(), reservation, cancel)
    }

    fn too_large(
        &self,
        segment: &Segment,
        reservation: &mut Reservation,
        cancel: &Cancel,
    ) -> Result<Hunk> {
        let mut lease = self.lease();
        let w = lease.worker.as_mut().expect("leased");
        resample::refine_large(w, segment, reservation.bytes(), cancel)
    }
}

/// Refines A lines `ra` against B lines `rb` within `reservation`, which
/// should hold [`estimate`] of their lengths.
fn refine<A: Source + Send + ?Sized, B: Source + Send + ?Sized>(
    w: &mut Worker<A, B>,
    ra: Range<LineNo>,
    rb: Range<LineNo>,
    reservation: &mut Reservation,
    cancel: &Cancel,
) -> Result<Hunk> {
    let ha = hashes(&w.a, ra.clone(), &mut w.buf)?;
    let hb = hashes(&w.b, rb.clone(), &mut w.buf)?;
    if cancel.is_set() {
        return Ok(Hunk::default());
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
    approx: bool,
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
            approx: false,
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

    /// Replaces A lines `ra` by B lines `rb`, marking the hunk approximate.
    fn replace(&mut self, ra: Range<LineNo>, rb: Range<LineNo>, spare: usize) -> Result<bool> {
        self.approx = true;
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
