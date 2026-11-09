//! Refinement scheduler with priorities, cancellation and a hunk cache.
//!
//! The scheduler owns the segment map and a [`Job`] that refines one segment
//! at a time. Work is ordered visible, then neighbours of the visible
//! window, then segments a front end asked for, then everything else in file
//! order. Moving the window deprioritizes what left it and cancels in-flight
//! prefetches that are no longer near it. Prefetch and background work
//! refine each segment once; an evicted hunk is recomputed only when asked
//! for again, so the cache must hold what a front end asks for in one frame.
//!
//! Memory: the whole scheduler lives in the Refine half of the [`Budget`],
//! in what is free of it at construction; a front end reserves its own
//! buffers first. The row table is reserved once, and what is left is split
//! in two equal parts: the cache, reserved at once, and `work_cap`, what
//! jobs reserve their working set from. So `sum(active jobs) + cache <= B/2`
//! holds by construction, a job whose estimate exceeds `work_cap` can never
//! run as is (it gets [`Job::too_large`] instead, with `work_cap` as its
//! limit), and the cache can hold any hunk a job is allowed to return.
//! A request for a hunk is a flag in the row table, so asking for every
//! segment adds no heap. `running`, which grows with the thread count, is
//! not charged. A hunk handed out through [`DiffView::hunk`]
//! or [`Scheduler::wait_hunk`] stays allocated while the front end holds
//! the `Arc`, after its charge was released by eviction; the terminal
//! interface keeps hunks only for the frame it draws, and the unified
//! writer only while it copies one edit out.
//!
//! Threads: a small fixed pool of worker threads, not rayon, so a worker
//! waiting for budget blocks nothing else.

mod cache;
#[cfg(test)]
mod tests;

use std::io;
use std::ops::Range;
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use crate::budget::{Budget, BudgetExceeded, Half, Reservation};
use crate::edit::{Edit, Hunk};
use crate::error::Result;
use crate::segment::{Segment, SegmentStatus};
use crate::types::{Bytes, LineNo};
use crate::view::{ApproxTally, DiffView, HunkState, Progress};
pub use cache::HUNK_OVERHEAD;
use cache::{Cache, State};

/// Segments on each side of the visible window that count as neighbours.
const NEIGHBORS: usize = 4;
/// How long a worker that found no room for the top job waits before it
/// tries again without being woken; see `Queue::retry`.
const RETRY: std::time::Duration = std::time::Duration::from_millis(50);

/// Cooperative cancellation token handed to a running job.
#[derive(Debug, Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    /// Whether the scheduler no longer wants the result.
    ///
    /// A job checks this at its natural checkpoints and returns early when it
    /// is set; whatever it returns is then discarded.
    #[must_use]
    pub fn is_set(&self) -> bool {
        // Relaxed: the flag orders no other memory, the job only needs to
        // see it eventually.
        self.0.load(Ordering::Relaxed)
    }

    fn set(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// What refines one segment. Results must depend on the segment alone, so
/// that an evicted hunk recomputes to the same script.
pub trait Job: Send + Sync + 'static {
    /// Peak working set of refining `segment`. Count at least
    /// `size_of::<Edit>()` per line plus [`HUNK_OVERHEAD`] so the finished
    /// hunk also fits the cache.
    fn estimate(&self, segment: &Segment) -> Bytes;

    /// Refines `segment`, number `index` in the map, within its estimate,
    /// which `reservation` holds, checking `cancel` along the way.
    ///
    /// # Errors
    ///
    /// Whatever stops the refinement, typically a failed read. The error
    /// goes to whoever waits for the hunk.
    fn run(
        &self,
        index: usize,
        segment: &Segment,
        reservation: &mut Reservation,
        cancel: &Cancel,
    ) -> Result<Hunk>;

    /// Called instead of [`run`](Job::run) when the estimate exceeds the
    /// most the scheduler can ever reserve for one job, which `reservation`
    /// holds. The job resamples within it or returns an approximate script;
    /// either way the hunk, counted like the cache counts it, stays within
    /// the reservation. A whole replacement, two edits, always does.
    ///
    /// # Errors
    ///
    /// As for [`run`](Job::run).
    fn too_large(
        &self,
        index: usize,
        segment: &Segment,
        reservation: &mut Reservation,
        cancel: &Cancel,
    ) -> Result<Hunk>;
}

/// Lazily refined diff over a fixed thread pool. Implements [`DiffView`].
#[derive(Debug)]
pub struct Scheduler<J: Job> {
    shared: Arc<Shared<J>>,
    workers: Vec<JoinHandle<()>>,
}

#[derive(Debug)]
struct Shared<J> {
    job: J,
    segments: Vec<Segment>,
    budget: Budget,
    work_cap: Bytes,
    total: u64,
    state: Mutex<Queue>,
    /// Idle workers wait here. Any one of them can take any job, so a
    /// change that makes work wakes one, and a worker that starts a job
    /// while more is ready wakes the next.
    work: Condvar,
    /// Callers of `wait_hunk` wait here, woken by every finished job.
    ready: Condvar,
}

#[derive(Debug)]
struct Running {
    segment: usize,
    /// Started because it was visible or a neighbour; the only kind that
    /// gets cancelled when the window moves away.
    prefetch: bool,
    cancel: Cancel,
}

#[derive(Debug)]
struct Queue {
    cache: Cache,
    _cache_block: Reservation,
    /// Segment indices overlapping the visible lines.
    visible: Range<usize>,
    /// No row below it is flagged as asked for through `hunk()`. Flagged
    /// rows are served before background, lowest index first.
    wanted_from: usize,
    /// How long a worker waits for bytes before it tries again unwoken.
    /// [`RETRY`]; tests lengthen it to see whether a wakeup came.
    retry: std::time::Duration,
    /// Background scan position in file order: nothing below it is fresh.
    cursor: usize,
    /// Whether idle workers refine segments nobody asked for.
    background: bool,
    running: Vec<Running>,
    /// Segments whose job failed, with the reason; one entry each.
    failures: Vec<(usize, Failure)>,
    /// First completions that came out approximate.
    approx: ApproxTally,
    stop: bool,
}

/// Why a job produced no hunk.
#[derive(Debug)]
enum Failure {
    Panicked,
    /// Kept whole, not as text, so a budget refusal reaches the front end
    /// with its numbers.
    Error(crate::Error),
}

impl Failure {
    /// The error for one waiter; every waiter of the segment gets its own.
    fn replay(&self, segment: usize) -> crate::Error {
        use crate::Error;
        let context = |what: &dyn std::fmt::Display| format!("refining segment {segment}: {what}");
        match self {
            Self::Panicked => io::Error::other(context(&"the job panicked")).into(),
            Self::Error(Error::Io(e)) => io::Error::new(e.kind(), context(e)).into(),
            Self::Error(Error::Write(e)) => Error::Write(io::Error::new(e.kind(), context(e))),
            Self::Error(Error::Budget(b)) => Error::Budget(*b),
            Self::Error(Error::LineTooLong { line, cap }) => Error::LineTooLong {
                line: *line,
                cap: *cap,
            },
            Self::Error(Error::LineOutOfRange { line, count }) => Error::LineOutOfRange {
                line: *line,
                count: *count,
            },
            Self::Error(Error::InvalidScript(s)) => Error::InvalidScript(s.clone()),
            Self::Error(Error::Keyed(k)) => Error::Keyed(k.clone()),
            Self::Error(Error::Cancelled) => Error::Cancelled,
        }
    }
}

/// The smallest free Refine half [`Scheduler::new`] accepts for
/// `segments` segments: the row table plus a cache and a work share of
/// [`HUNK_OVERHEAD`] and two edits each.
pub(crate) fn least_bytes(segments: usize) -> Bytes {
    let share = HUNK_OVERHEAD.0 + 2 * size_of::<Edit>() as u64;
    Bytes(Cache::table_bytes(segments).0 + 2 * share)
}

impl<J: Job> Scheduler<J> {
    /// Starts `threads` workers over `segments`, which must tile both files.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Budget`](crate::Error::Budget) when the free part
    /// of the Refine half cannot hold the row table plus a cache and a work
    /// share of [`HUNK_OVERHEAD`] and two edits each, and
    /// [`Error::Io`](crate::Error::Io) when a worker thread cannot be
    /// spawned.
    pub fn new(job: J, segments: Vec<Segment>, budget: &Budget, threads: usize) -> Result<Self> {
        let free = budget.available(Half::Refine);
        let table = Cache::table_bytes(segments.len());
        let share = free.0.saturating_sub(table.0) / 2;
        let least = least_bytes(segments.len());
        if free < least {
            return Err(BudgetExceeded {
                half: Half::Refine,
                requested: least,
                available: free,
            }
            .into());
        }
        let work_cap = Bytes(share);
        let reservation = budget.try_reserve(Half::Refine, Bytes(table.0 + share))?;
        let equal = |i: usize| segments[i].status == SegmentStatus::Equal;
        let cache = Cache::new(segments.len(), equal, Bytes(share));
        let total = segments
            .iter()
            .filter(|s| s.status != SegmentStatus::Equal)
            .count() as u64;
        let shared = Arc::new(Shared {
            job,
            segments,
            budget: budget.clone(),
            work_cap,
            total,
            state: Mutex::new(Queue {
                cache,
                _cache_block: reservation,
                visible: 0..0,
                wanted_from: 0,
                retry: RETRY,
                cursor: 0,
                background: true,
                running: Vec::with_capacity(threads),
                failures: Vec::new(),
                approx: ApproxTally::default(),
                stop: false,
            }),
            work: Condvar::new(),
            ready: Condvar::new(),
        });
        let workers = (0..threads.max(1))
            .map(|_| {
                let shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name("rift-sched".into())
                    .spawn(move || worker(&shared))
            })
            .collect::<io::Result<_>>()?;
        Ok(Scheduler { shared, workers })
    }

    /// The most one job may reserve; estimates above it go to
    /// [`Job::too_large`].
    #[must_use]
    pub fn work_cap(&self) -> Bytes {
        self.shared.work_cap
    }

    /// Turns background refinement on or off; it is on from the start.
    ///
    /// The unified writer turns it off and moves the visible window ahead
    /// of itself instead: background work runs ahead without bound, and
    /// once the cache is full it evicts hunks the writer has not reached,
    /// which then have to be computed twice.
    pub fn set_background(&self, on: bool) {
        self.shared.lock().background = on;
        self.shared.work.notify_one();
    }

    /// [`DiffView::hunk`] that reports a failed job as its error instead of
    /// panicking. Never blocks; a segment not refined yet is queued.
    ///
    /// # Errors
    ///
    /// As for [`wait_hunk`](Self::wait_hunk).
    ///
    /// # Panics
    ///
    /// Panics if `segment` is out of range.
    pub fn try_hunk(&self, segment: usize) -> Result<HunkState> {
        if let Some(hunk) = self.shared.equal_hunk(segment) {
            return Ok(HunkState::Ready(hunk));
        }
        Ok(
            match self.shared.lookup(&mut self.shared.lock(), segment)? {
                Some(hunk) if hunk.approx.is_some() => HunkState::Approx(hunk),
                Some(hunk) => HunkState::Ready(hunk),
                None => HunkState::Pending,
            },
        )
    }

    /// Whether `segment` has been refined and, if so, whether the result is
    /// approximate. Never queues a job and never blocks on one; `None` for a
    /// segment not refined yet, one that failed, or an equal one.
    ///
    /// # Panics
    ///
    /// Panics if `segment` is out of range.
    #[must_use]
    pub fn refined_approx(&self, segment: usize) -> Option<bool> {
        self.shared.lock().cache.refined_approx(segment)
    }

    /// The hunk for `segment`, computing it first if needed. Blocks; for the
    /// unified writer and tests, not for a frame loop.
    ///
    /// # Errors
    ///
    /// The job's error, or an error naming the panic if the job panicked.
    ///
    /// # Panics
    ///
    /// Panics if `segment` is out of range.
    pub fn wait_hunk(&self, segment: usize) -> Result<Arc<Hunk>> {
        if let Some(hunk) = self.shared.equal_hunk(segment) {
            return Ok(hunk);
        }
        let mut q = self.shared.lock();
        loop {
            if let Some(hunk) = self.shared.lookup(&mut q, segment)? {
                return Ok(hunk);
            }
            q = wait(&self.shared.ready, q);
        }
    }
}

impl<J: Job> Drop for Scheduler<J> {
    fn drop(&mut self) {
        {
            let mut q = self.shared.lock();
            q.stop = true;
            for r in &q.running {
                r.cancel.set();
            }
        }
        self.shared.work.notify_all();
        self.shared.ready.notify_all();
        for w in self.workers.drain(..) {
            // A worker only dies with a panicking job; its state is already
            // out of the queue, nothing to recover here.
            let _ = w.join();
        }
    }
}

impl<J: Job> DiffView for Scheduler<J> {
    fn len_a(&self) -> u64 {
        self.shared.segments.last().map_or(0, |s| s.a.end.0)
    }

    fn len_b(&self) -> u64 {
        self.shared.segments.last().map_or(0, |s| s.b.end.0)
    }

    fn segments(&self) -> &[Segment] {
        &self.shared.segments
    }

    fn hunk(&self, segment: usize) -> HunkState {
        // A frame loop has no error channel; a failed segment is a read
        // error or a bug, and the front end stops with its message.
        self.try_hunk(segment).unwrap_or_else(|e| panic!("{e}"))
    }

    fn progress(&self) -> Progress {
        Progress {
            done: self.shared.lock().cache.done(),
            total: self.shared.total,
        }
    }

    fn approx_refined(&self) -> ApproxTally {
        self.shared.lock().approx
    }

    fn set_visible(&self, range: Range<LineNo>) {
        let visible = visible_segments(&self.shared.segments, range);
        {
            let mut q = self.shared.lock();
            if q.visible == visible {
                return;
            }
            q.visible = visible;
            for r in &q.running {
                if r.prefetch && !q.near().any(|i| i == r.segment) {
                    r.cancel.set();
                }
            }
        }
        self.shared.work.notify_one();
    }
}

impl<J: Job> Shared<J> {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The one-edit script of an equal segment, built without a job.
    fn equal_hunk(&self, segment: usize) -> Option<Arc<Hunk>> {
        let s = &self.segments[segment];
        (s.status == SegmentStatus::Equal).then(|| {
            Arc::new(Hunk {
                edits: vec![Edit::Equal {
                    a: s.a.start,
                    b: s.b.start,
                    len: s.len_a(),
                }],
                approx: None,
            })
        })
    }

    /// The cached hunk, or a request for it that wakes a worker. A failed
    /// job is reported instead, so its waiter does not wait forever.
    fn lookup(&self, q: &mut Queue, segment: usize) -> Result<Option<Arc<Hunk>>> {
        if q.cache.state(segment) == State::Failed {
            let failure = q.failures.iter().find(|f| f.0 == segment);
            return Err(failure.map_or_else(
                || io::Error::other(format!("refining segment {segment}: failed")).into(),
                |f| f.1.replay(segment),
            ));
        }
        let hunk = q.cache.get(segment);
        if hunk.is_none() && q.cache.state(segment) == State::Idle {
            q.want(segment);
            // Also when it was wanted already: a worker that found no
            // room for it may be waiting with nothing running to wake it.
            self.work.notify_one();
        }
        Ok(hunk)
    }
}

impl Queue {
    /// Visible segments, then the neighbours after them, then before.
    fn near(&self) -> impl Iterator<Item = usize> {
        let v = self.visible.clone();
        let (after, before) = if v.is_empty() {
            (0..0, 0..0)
        } else {
            (
                v.end..(v.end + NEIGHBORS).min(self.cache.len()),
                v.start.saturating_sub(NEIGHBORS)..v.start,
            )
        };
        v.chain(after).chain(before.rev())
    }

    /// Queues `segment` for [`pick`](Self::pick), ahead of background work.
    /// Returns whether it was not queued yet.
    fn want(&mut self, segment: usize) -> bool {
        self.wanted_from = self.wanted_from.min(segment);
        self.cache.want(segment)
    }

    /// The next segment to refine and whether it is a prefetch. Reads only,
    /// except for clearing stale `wanted` flags and advancing the cursors
    /// over segments that need no job.
    ///
    /// Prefetch and the cursor refine each segment once; an evicted hunk
    /// comes back only on request, so a near set larger than the cache
    /// cannot thrash.
    fn pick(&mut self) -> Option<(usize, bool)> {
        if let Some(i) = self.near().find(|&i| self.cache.fresh(i)) {
            return Some((i, true));
        }
        while let Some(i) = self.cache.next_wanted(self.wanted_from) {
            self.wanted_from = i;
            if self.cache.state(i) == State::Idle {
                return Some((i, false));
            }
            self.cache.unwant(i);
        }
        self.wanted_from = self.cache.len();
        while self.background && self.cursor < self.cache.len() {
            if self.cache.fresh(self.cursor) {
                return Some((self.cursor, false));
            }
            self.cursor += 1;
        }
        None
    }

    fn start(&mut self, segment: usize, prefetch: bool) -> Cancel {
        let cancel = Cancel::default();
        self.cache.set_state(segment, State::Running);
        self.running.push(Running {
            segment,
            prefetch,
            cancel: cancel.clone(),
        });
        cancel
    }

    fn fail(&mut self, segment: usize, why: Failure) {
        let at = self.running.iter().position(|r| r.segment == segment);
        self.running
            .swap_remove(at.expect("failed job was running"));
        self.cache.set_state(segment, State::Failed);
        self.failures.push((segment, why));
    }

    fn complete(&mut self, segment: usize, seg: &Segment, hunk: Hunk, cancelled: bool) {
        let at = self.running.iter().position(|r| r.segment == segment);
        self.running
            .swap_remove(at.expect("completed job was running"));
        if cancelled {
            self.cache.set_state(segment, State::Idle);
            self.cursor = self.cursor.min(segment);
            return;
        }
        if self.cache.mark_done(segment) && hunk.approx.is_some() {
            self.cache.mark_approx(segment);
            self.approx.segments += 1;
            self.approx.a_lines += seg.len_a();
            self.approx.b_lines += seg.len_b();
        }
        // A hunk larger than the whole cache would be dropped and a waiter
        // would recompute it forever. The cache is as large as work_cap and
        // a job's hunk stays within its reservation, so this cannot happen
        // while jobs keep to the estimate rule.
        let kept = self.cache.insert(segment, Arc::new(hunk));
        debug_assert!(kept, "hunk of segment {segment} exceeds the cache");
    }
}

fn wait<'a>(cv: &Condvar, q: MutexGuard<'a, Queue>) -> MutexGuard<'a, Queue> {
    cv.wait(q).unwrap_or_else(PoisonError::into_inner)
}

fn worker<J: Job>(shared: &Shared<J>) {
    let mut q = shared.lock();
    loop {
        if q.stop {
            return;
        }
        let Some((i, prefetch)) = q.pick() else {
            q = wait(&shared.work, q);
            continue;
        };
        let segment = &shared.segments[i];
        let estimate = shared.job.estimate(segment);
        let too_large = estimate > shared.work_cap;
        let Ok(reservation) = shared
            .budget
            .try_reserve(Half::Refine, estimate.min(shared.work_cap))
        else {
            // Strict priority: the top job waits for a running one to give
            // its bytes back rather than letting a smaller one jump ahead.
            debug_assert!(
                !q.running.is_empty(),
                "Refine half is used outside the scheduler"
            );
            // Timed: bytes held outside the scheduler come back with no
            // notify, and a blocked `wait_hunk` would wait with it.
            let retry = q.retry;
            q = shared
                .work
                .wait_timeout(q, retry)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            continue;
        };
        let cancel = q.start(i, prefetch);
        if q.pick().is_some() {
            shared.work.notify_one();
        }
        drop(q);

        // A panicking job must not take the worker down with it and leave
        // its segment Running forever; the panic is passed to whoever asks
        // for the hunk. Nothing the job touched is shared but the budget,
        // which the reservation's drop settles.
        let mut reservation = reservation;
        let out = panic::catch_unwind(AssertUnwindSafe(|| {
            if too_large {
                shared.job.too_large(i, segment, &mut reservation, &cancel)
            } else {
                shared.job.run(i, segment, &mut reservation, &cancel)
            }
        }));
        drop(reservation);

        q = shared.lock();
        match out {
            Ok(Ok(mut hunk)) => {
                hunk.edits.shrink_to_fit();
                q.complete(i, segment, hunk, cancel.is_set());
            }
            // A cancelled job may stop with an error; nobody wants it.
            Ok(Err(_)) if cancel.is_set() => q.complete(i, segment, Hunk::default(), true),
            Ok(Err(e)) => q.fail(i, Failure::Error(e)),
            Err(_) => q.fail(i, Failure::Panicked),
        }
        shared.ready.notify_all();
        shared.work.notify_one();
    }
}

/// Indices of the segments that show lines of A in `range`. A segment with
/// no A lines (a pure insert at A line `x`) counts with line `x - 1`, or
/// with line 0 at the top of the file.
fn visible_segments(segments: &[Segment], range: Range<LineNo>) -> Range<usize> {
    if range.is_empty() {
        return 0..0;
    }
    let lo =
        segments.partition_point(|s| s.a.end.0.max(u64::from(s.a.is_empty())) <= range.start.0);
    let hi = segments.partition_point(|s| s.a.start.0 < range.end.0 + u64::from(s.a.is_empty()));
    lo..hi.max(lo)
}
