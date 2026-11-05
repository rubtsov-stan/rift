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
//! Memory: the whole scheduler lives in the Refine half of the [`Budget`].
//! Half of that half is reserved once at construction as the cache block;
//! it pays for the per-segment row table and for every cached hunk. The
//! other half, `work_cap`, is what jobs reserve their working set from, so
//! `sum(active jobs) + cache <= B/2` holds by construction and a job whose
//! estimate exceeds `work_cap` can never run as is: it gets
//! [`Job::too_large`] instead, with `work_cap` as its limit. Bookkeeping that
//! grows with threads and pending requests (`running`, `wanted`) is not
//! charged. A hunk handed out through [`DiffView::hunk`] stays allocated
//! while the front end holds the `Arc`, after its charge was released by
//! eviction; a front end keeps hunks only for the frame it draws.
//!
//! Threads: a small fixed pool of worker threads, not rayon, so a worker
//! waiting for budget blocks nothing else.

mod cache;
#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;

use crate::budget::{Budget, Half, Reservation};
use crate::edit::{Edit, Hunk};
use crate::error::Result;
use crate::segment::{Segment, SegmentStatus};
use crate::types::{Bytes, LineNo};
use crate::view::{DiffView, HunkState, Progress};
use cache::{Cache, State};

/// Segments on each side of the visible window that count as neighbours.
const NEIGHBORS: usize = 4;

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
    /// `size_of::<Edit>()` per line so the finished hunk also fits the cache.
    fn estimate(&self, segment: &Segment) -> Bytes;

    /// Refines `segment` within its estimate, checking `cancel` along the way.
    fn run(&self, segment: &Segment, cancel: &Cancel) -> Hunk;

    /// Called instead of [`run`](Job::run) when the estimate exceeds `limit`,
    /// the most the scheduler can ever reserve for one job. The job resamples
    /// within `limit` or returns an approximate script.
    fn too_large(&self, segment: &Segment, limit: Bytes, cancel: &Cancel) -> Hunk;
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
    cv: Condvar,
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
    /// Asked for through `hunk()` and not cached: served before background.
    wanted: BTreeSet<usize>,
    /// Background scan position in file order: nothing below it is fresh.
    cursor: usize,
    running: Vec<Running>,
    stop: bool,
}

impl<J: Job> Scheduler<J> {
    /// Starts `threads` workers over `segments`, which must tile both files.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Budget`](crate::Error::Budget) when the Refine half
    /// cannot hold the cache block, and [`Error::Io`](crate::Error::Io) when
    /// a worker thread cannot be spawned.
    pub fn new(job: J, segments: Vec<Segment>, budget: &Budget, threads: usize) -> Result<Self> {
        let half = budget.capacity(Half::Refine);
        let cache_block = Bytes(half.0 / 2);
        let work_cap = Bytes(half.0 - cache_block.0);
        let reservation = budget.try_reserve(Half::Refine, cache_block)?;
        let table = Cache::table_bytes(segments.len());
        let cache_cap = Bytes(cache_block.0.saturating_sub(table.0));
        let equal = |i: usize| segments[i].status == SegmentStatus::Equal;
        let cache = Cache::new(segments.len(), equal, cache_cap);
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
                wanted: BTreeSet::new(),
                cursor: 0,
                running: Vec::with_capacity(threads),
                stop: false,
            }),
            cv: Condvar::new(),
        });
        let workers = (0..threads.max(1))
            .map(|_| {
                let shared = Arc::clone(&shared);
                std::thread::Builder::new()
                    .name("rift-sched".into())
                    .spawn(move || worker(&shared))
            })
            .collect::<std::io::Result<_>>()?;
        Ok(Scheduler { shared, workers })
    }

    /// The most one job may reserve; estimates above it go to
    /// [`Job::too_large`].
    #[must_use]
    pub fn work_cap(&self) -> Bytes {
        self.shared.work_cap
    }

    /// The hunk for `segment`, computing it first if needed. Blocks; for the
    /// unified writer and tests, not for a frame loop.
    ///
    /// # Panics
    ///
    /// Panics if `segment` is out of range.
    #[allow(
        clippy::must_use_candidate,
        reason = "waiting for the computation is the effect; the value may be ignored"
    )]
    pub fn wait_hunk(&self, segment: usize) -> Arc<Hunk> {
        if let Some(hunk) = self.shared.equal_hunk(segment) {
            return hunk;
        }
        let mut q = self.shared.lock();
        loop {
            if let Some(hunk) = self.shared.lookup(&mut q, segment) {
                return hunk;
            }
            q = self.shared.wait(q);
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
        self.shared.cv.notify_all();
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
        if let Some(hunk) = self.shared.equal_hunk(segment) {
            return HunkState::Ready(hunk);
        }
        match self.shared.lookup(&mut self.shared.lock(), segment) {
            Some(hunk) if hunk.approx => HunkState::Approx(hunk),
            Some(hunk) => HunkState::Ready(hunk),
            None => HunkState::Pending,
        }
    }

    fn progress(&self) -> Progress {
        Progress {
            done: self.shared.lock().cache.done(),
            total: self.shared.total,
        }
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
        self.shared.cv.notify_all();
    }
}

impl<J: Job> Shared<J> {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'a>(&'a self, q: MutexGuard<'a, Queue>) -> MutexGuard<'a, Queue> {
        self.cv.wait(q).unwrap_or_else(PoisonError::into_inner)
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
                approx: false,
            })
        })
    }

    /// The cached hunk, or a request for it that wakes a worker.
    fn lookup(&self, q: &mut Queue, segment: usize) -> Option<Arc<Hunk>> {
        let hunk = q.cache.get(segment);
        if hunk.is_none() && q.cache.state(segment) == State::Idle && q.wanted.insert(segment) {
            self.cv.notify_all();
        }
        hunk
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

    /// The next segment to refine and whether it is a prefetch. Reads only,
    /// except for dropping stale `wanted` entries and advancing the cursor
    /// over segments that need no job.
    ///
    /// Prefetch and the cursor refine each segment once; an evicted hunk
    /// comes back only on request, so a near set larger than the cache
    /// cannot thrash.
    fn pick(&mut self) -> Option<(usize, bool)> {
        if let Some(i) = self.near().find(|&i| self.cache.fresh(i)) {
            return Some((i, true));
        }
        while let Some(&i) = self.wanted.first() {
            if self.cache.state(i) == State::Idle {
                return Some((i, false));
            }
            self.wanted.pop_first();
        }
        while self.cursor < self.cache.len() {
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

    fn complete(&mut self, segment: usize, hunk: Hunk, cancelled: bool) {
        let at = self.running.iter().position(|r| r.segment == segment);
        self.running
            .swap_remove(at.expect("completed job was running"));
        if cancelled {
            self.cache.set_state(segment, State::Idle);
            self.cursor = self.cursor.min(segment);
            return;
        }
        self.cache.mark_done(segment);
        // A hunk larger than the whole cache is dropped and the
        // segment stays Idle, so a front end that keeps asking recomputes it
        // every time. Unreachable while estimates count size_of::<Edit>()
        // per line, since work_cap == cache block.
        self.cache.insert(segment, Arc::new(hunk));
    }
}

fn worker<J: Job>(shared: &Shared<J>) {
    let mut q = shared.lock();
    loop {
        if q.stop {
            return;
        }
        let Some((i, prefetch)) = q.pick() else {
            q = shared.wait(q);
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
            q = shared.wait(q);
            continue;
        };
        let cancel = q.start(i, prefetch);
        drop(q);

        let mut hunk = if too_large {
            shared.job.too_large(segment, shared.work_cap, &cancel)
        } else {
            shared.job.run(segment, &cancel)
        };
        drop(reservation);
        hunk.edits.shrink_to_fit();

        q = shared.lock();
        q.complete(i, hunk, cancel.is_set());
        shared.cv.notify_all();
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
