use std::ops::Range;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use super::cache::hunk_bytes;
use super::*;
use crate::edit::ApproxReason;

const LINES: u64 = 10;

/// `n` segments of `LINES` lines each; segment `i` is `Equal` iff `equal(i)`.
fn segments(n: usize, equal: impl Fn(usize) -> bool) -> Vec<Segment> {
    (0..n)
        .map(|i| {
            let start = LineNo(i as u64 * LINES);
            Segment {
                a: start..start + LINES,
                b: start..start + LINES,
                status: if equal(i) {
                    SegmentStatus::Equal
                } else {
                    SegmentStatus::Differs
                },
            }
        })
        .collect()
}

fn id(segment: &Segment) -> usize {
    usize::try_from(segment.a.start.0 / LINES).unwrap()
}

fn lines(segment: usize) -> Range<LineNo> {
    let start = LineNo(segment as u64 * LINES);
    start..start + LINES
}

fn script(segment: &Segment, approx: bool) -> Hunk {
    Hunk {
        edits: vec![
            Edit::Delete {
                a: segment.a.start,
                len: segment.len_a(),
            },
            Edit::Insert {
                b: segment.b.start,
                len: segment.len_b(),
            },
        ],
        approx: approx.then_some(ApproxReason::Budget),
    }
}

type Estimate = Box<dyn Fn(&Segment) -> Bytes + Send + Sync>;

/// A job that reports every start, can be held at a gate or spun until
/// cancelled, and reports whether it saw its cancellation token.
struct Fake {
    estimate: Estimate,
    started: Option<Sender<usize>>,
    gate: Option<Mutex<Receiver<()>>>,
    spin_until_cancel: bool,
    seen_cancel: Option<Sender<(usize, bool)>>,
    calls: Mutex<Vec<usize>>,
    too_large: Mutex<Vec<(usize, Bytes)>>,
}

impl Fake {
    fn new() -> Self {
        Fake {
            estimate: Box::new(|_| Bytes(8)),
            started: None,
            gate: None,
            spin_until_cancel: false,
            seen_cancel: None,
            calls: Mutex::new(Vec::new()),
            too_large: Mutex::new(Vec::new()),
        }
    }

    fn enter(&self, segment: &Segment, cancel: &Cancel) {
        let i = id(segment);
        self.calls.lock().unwrap().push(i);
        if let Some(s) = &self.started {
            s.send(i).unwrap();
        }
        if let Some(g) = &self.gate {
            g.lock().unwrap().recv().unwrap();
        }
        while self.spin_until_cancel && !cancel.is_set() {
            std::thread::yield_now();
        }
        if let Some(s) = &self.seen_cancel {
            s.send((i, cancel.is_set())).unwrap();
        }
    }
}

impl Job for Fake {
    fn estimate(&self, segment: &Segment) -> Bytes {
        (self.estimate)(segment)
    }

    fn run(
        &self,
        _: usize,
        segment: &Segment,
        _: &mut Reservation,
        cancel: &Cancel,
    ) -> Result<Hunk> {
        self.enter(segment, cancel);
        Ok(script(segment, false))
    }

    fn too_large(
        &self,
        _: usize,
        segment: &Segment,
        reservation: &mut Reservation,
        cancel: &Cancel,
    ) -> Result<Hunk> {
        let limit = reservation.bytes();
        self.too_large.lock().unwrap().push((id(segment), limit));
        self.enter(segment, cancel);
        Ok(script(segment, true))
    }
}

/// A budget whose cache holds exactly `hunks` scripts of the fake job; the
/// work cap is the same size as the cache.
fn budget(n: usize, hunks: u64) -> Budget {
    let one = hunk_bytes(&script(&segments(1, |_| false)[0], false));
    let half = Cache::table_bytes(n).0 + 2 * (hunks * one + one / 2);
    Budget::new(Bytes(2 * half))
}

fn wait_all<J: Job>(s: &Scheduler<J>) {
    for i in 0..s.segments().len() {
        s.wait_hunk(i).unwrap();
    }
}

fn ready(state: &HunkState) -> Option<&Arc<Hunk>> {
    match state {
        HunkState::Ready(h) => Some(h),
        _ => None,
    }
}

#[test]
fn visible_segments_maps_lines_to_segments() {
    let seg = |a: Range<u64>, b: Range<u64>| Segment {
        a: LineNo(a.start)..LineNo(a.end),
        b: LineNo(b.start)..LineNo(b.end),
        status: SegmentStatus::Differs,
    };
    // 0: 0..10, 1: insert at A line 10, 2: 10..20, 3: insert at 20.
    let segs = vec![
        seg(0..10, 0..10),
        seg(10..10, 10..15),
        seg(10..20, 15..25),
        seg(20..20, 25..30),
    ];
    let vis = |r: Range<u64>| visible_segments(&segs, LineNo(r.start)..LineNo(r.end));
    assert_eq!(vis(0..5), 0..1);
    assert_eq!(vis(0..10), 0..2, "insert at 10 belongs to line 9");
    assert_eq!(vis(10..12), 2..3);
    assert_eq!(vis(9..11), 0..3);
    assert_eq!(vis(15..20), 2..4, "trailing insert belongs to line 19");
    assert_eq!(vis(20..30), 4..4);
    assert_eq!(vis(5..5), 0..0);
}

#[test]
fn equal_segments_are_ready_without_a_job() {
    let n = 4;
    let s = Scheduler::new(Fake::new(), segments(n, |i| i % 2 == 0), &budget(n, 4), 1).unwrap();
    assert_eq!(s.len_a(), 40);
    assert_eq!(s.len_b(), 40);
    let h = ready(&s.hunk(0)).cloned().expect("equal is ready at once");
    assert_eq!(
        h.edits,
        [Edit::Equal {
            a: LineNo(0),
            b: LineNo(0),
            len: LINES
        }]
    );
    wait_all(&s);
    assert_eq!(s.progress(), Progress { done: 2, total: 2 });
    assert_eq!(*s.shared.job.calls.lock().unwrap(), [1, 3]);
}

#[test]
fn visible_then_neighbours_then_background_order() {
    let n = 24;
    let (started, starts) = channel();
    let (release, gate) = channel();
    let job = Fake {
        started: Some(started),
        gate: Some(Mutex::new(gate)),
        ..Fake::new()
    };
    let s = Scheduler::new(job, segments(n, |_| false), &budget(n, 64), 1).unwrap();
    // Background starts at 0 and is held at the gate while the window moves.
    assert_eq!(starts.recv().unwrap(), 0);
    s.set_visible(lines(10).start..lines(11).end);
    release.send(()).unwrap();
    let mut order = vec![0];
    for _ in 1..n {
        order.push(starts.recv().unwrap());
        release.send(()).unwrap();
    }
    let expected: Vec<usize> = [0, 10, 11, 12, 13, 14, 15, 9, 8, 7, 6]
        .into_iter()
        .chain(1..6)
        .chain(16..24)
        .collect();
    assert_eq!(order, expected);
    wait_all(&s);
    assert_eq!(
        s.progress(),
        Progress {
            done: 24,
            total: 24
        }
    );
}

#[test]
fn requested_hunk_is_served_before_background() {
    let n = 8;
    let (started, starts) = channel();
    let (release, gate) = channel();
    let job = Fake {
        started: Some(started),
        gate: Some(Mutex::new(gate)),
        ..Fake::new()
    };
    let s = Scheduler::new(job, segments(n, |_| false), &budget(n, 64), 1).unwrap();
    assert_eq!(starts.recv().unwrap(), 0);
    assert!(matches!(s.hunk(6), HunkState::Pending));
    release.send(()).unwrap();
    assert_eq!(starts.recv().unwrap(), 6, "asked for while idle");
    release.send(()).unwrap();
    assert_eq!(starts.recv().unwrap(), 1, "then the cursor resumes");
    for _ in 0..n - 2 {
        release.send(()).unwrap();
    }
    s.wait_hunk(7).unwrap();
    assert!(ready(&s.hunk(6)).is_some());
}

/// A guard on the order `pick` serves requests in; it passes on the
/// ordered set this replaced as well.
#[test]
fn requests_are_served_lowest_segment_first() {
    let n = 10;
    let (started, starts) = channel();
    let (release, gate) = channel();
    let job = Fake {
        started: Some(started),
        gate: Some(Mutex::new(gate)),
        ..Fake::new()
    };
    let s = Scheduler::new(job, segments(n, |_| false), &budget(n, 64), 1).unwrap();
    assert_eq!(starts.recv().unwrap(), 0);
    s.set_background(false);
    for i in [7, 3, 5] {
        assert!(matches!(s.hunk(i), HunkState::Pending));
    }
    release.send(()).unwrap();
    assert_eq!(starts.recv().unwrap(), 3);
    // Below the one running, and past the last one asked for.
    for i in [9, 1, 7] {
        assert!(matches!(s.hunk(i), HunkState::Pending));
    }
    let mut order = Vec::new();
    for _ in 0..4 {
        release.send(()).unwrap();
        order.push(starts.recv().unwrap());
    }
    assert_eq!(order, [1, 5, 7, 9]);
    release.send(()).unwrap();
    assert!(
        starts
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "each request ran once"
    );
}

#[test]
fn prefetch_that_left_the_window_is_cancelled_and_redone() {
    let n = 20;
    let (started, starts) = channel();
    let (release, gate) = channel();
    let (seen, cancels) = channel();
    let job = Fake {
        started: Some(started),
        gate: Some(Mutex::new(gate)),
        seen_cancel: Some(seen),
        ..Fake::new()
    };
    let s = Scheduler::new(job, segments(n, |_| false), &budget(n, 64), 1).unwrap();
    assert_eq!(starts.recv().unwrap(), 0);
    s.set_visible(lines(5));
    // Background job 0 is not a prefetch: the window moving does not touch it.
    release.send(()).unwrap();
    assert_eq!(cancels.recv().unwrap(), (0, false));

    assert_eq!(starts.recv().unwrap(), 5);
    s.set_visible(lines(15));
    release.send(()).unwrap();
    assert_eq!(
        cancels.recv().unwrap(),
        (5, true),
        "token set before the job returned"
    );

    assert_eq!(starts.recv().unwrap(), 15);
    assert_eq!(
        s.progress(),
        Progress { done: 1, total: 20 },
        "result of the cancelled job was discarded"
    );
    release.send(()).unwrap();
    assert_eq!(cancels.recv().unwrap(), (15, false));
    for _ in 0..n {
        release.send(()).unwrap();
    }
    wait_all(&s);
    assert_eq!(
        s.progress(),
        Progress {
            done: 20,
            total: 20
        }
    );
    let rest: Vec<usize> = std::iter::from_fn(|| starts.try_recv().ok()).collect();
    assert_eq!(&rest[..8], &[16, 17, 18, 19, 14, 13, 12, 11]);
    let calls = s.shared.job.calls.lock().unwrap();
    assert_eq!(calls.iter().filter(|&&i| i == 5).count(), 2);
}

#[test]
fn queue_rewinds_cursor_after_a_cancel() {
    let n = 6;
    let b = Budget::new(Bytes(1024));
    let mut q = Queue {
        cache: Cache::new(n, |i| i >= 3, Bytes(512)),
        _cache_block: b.try_reserve(Half::Refine, Bytes(0)).unwrap(),
        visible: 2..3,
        wanted_from: 0,
        cursor: 0,
        background: true,
        running: Vec::new(),
        failures: Vec::new(),
        approx: ApproxTally::default(),
        stop: false,
    };
    let seg = &segments(1, |_| false)[0];
    assert_eq!(q.pick(), Some((2, true)));
    q.start(2, true);
    for i in [1, 0] {
        assert_eq!(q.pick(), Some((i, true)), "neighbours before the window");
        q.start(i, true);
        q.complete(i, seg, Hunk::default(), false);
    }
    assert_eq!(q.pick(), None, "cursor passed the running 2");
    q.visible = 0..0;
    q.complete(2, seg, Hunk::default(), true);
    assert_eq!(q.pick(), Some((2, false)), "cursor came back for it");
    assert_eq!(q.cache.done(), 2);
}

#[test]
fn a_far_request_does_not_make_every_pick_walk_to_it() {
    let n = 1 << 21;
    let b = Budget::new(Bytes(1024));
    let mut q = Queue {
        cache: Cache::new(n, |_| false, Bytes(u64::MAX)),
        _cache_block: b.try_reserve(Half::Refine, Bytes(0)).unwrap(),
        visible: 0..0,
        wanted_from: 0,
        cursor: 0,
        background: false,
        running: Vec::new(),
        failures: Vec::new(),
        approx: ApproxTally::default(),
        stop: false,
    };
    let seg = &segments(1, |_| false)[0];
    q.want(n - 1);
    let mut worst = 0;
    for k in 0..300 {
        q.want(k);
        q.cache.visits.set(0);
        assert_eq!(q.pick(), Some((k, false)));
        q.start(k, false);
        // The worker peeks once more after starting a job.
        assert_eq!(q.pick(), Some((n - 1, false)));
        worst = worst.max(q.cache.visits.get());
        q.complete(k, seg, Hunk::default(), false);
    }
    // A word per 4096 rows, and two groups of 64 rows.
    let bound = (n / 4096 + 2 * 64) as u64 * 2;
    assert!(
        worst <= bound,
        "{worst} rows and words looked at in one pick, bound {bound}"
    );
}

#[test]
fn lru_evicts_oldest_and_recompute_is_deterministic() {
    let n = 3;
    let (release, gate) = channel();
    let job = Fake {
        gate: Some(Mutex::new(gate)),
        ..Fake::new()
    };
    let s = Scheduler::new(job, segments(n, |_| false), &budget(n, 2), 1).unwrap();
    // One release per computation keeps the worker in step with the test.
    let step = |i: usize| {
        release.send(()).unwrap();
        s.wait_hunk(i).unwrap()
    };
    let h0 = step(0);
    let h1 = step(1);
    assert!(ready(&s.hunk(0)).is_some(), "touch makes 0 the most recent");
    let h2 = step(2);
    assert!(ready(&s.hunk(0)).is_some());
    assert!(ready(&s.hunk(2)).is_some());
    assert!(matches!(s.hunk(1), HunkState::Pending), "1 was the LRU");
    assert_eq!(
        s.progress(),
        Progress { done: 3, total: 3 },
        "eviction does not undo progress"
    );

    let again = step(1);
    assert_eq!(*again, *h1, "recomputed hunk equals the first");
    assert!(!Arc::ptr_eq(&again, &h1));
    assert!(
        matches!(s.hunk(0), HunkState::Pending),
        "0 was the LRU for the recompute"
    );
    assert_eq!(*step(0), *h0);
    assert_eq!(*step(2), *h2);
    assert_eq!(s.progress(), Progress { done: 3, total: 3 });
    assert_eq!(*s.shared.job.calls.lock().unwrap(), [0, 1, 2, 1, 0, 2]);
}

#[test]
fn too_large_jobs_get_the_fallback_and_are_marked_approx() {
    let n = 3;
    let job = Fake {
        estimate: Box::new(|s| if id(s) == 1 { Bytes(1 << 20) } else { Bytes(8) }),
        ..Fake::new()
    };
    let s = Scheduler::new(job, segments(n, |_| false), &budget(n, 4), 2).unwrap();
    let limit = s.work_cap();
    wait_all(&s);
    assert!(matches!(s.hunk(1), HunkState::Approx(_)));
    assert!(ready(&s.hunk(0)).is_some());
    assert_eq!(*s.shared.job.too_large.lock().unwrap(), [(1, limit)]);
    let one = ApproxTally {
        segments: 1,
        a_lines: LINES,
        b_lines: LINES,
    };
    assert_eq!(s.approx_refined(), one);
    assert_eq!(s.refined_approx(0), Some(false));
    assert_eq!(s.refined_approx(1), Some(true));
}

#[test]
fn asking_whether_a_segment_is_approx_queues_nothing() {
    let n = 3;
    let s = Scheduler::new(Fake::new(), segments(n, |_| false), &budget(n, 4), 1).unwrap();
    for i in 0..n {
        let _ = s.refined_approx(i);
    }
    assert!(s.shared.lock().cache.next_wanted(0).is_none());
}

#[test]
fn drop_cancels_running_jobs_and_joins() {
    let n = 4;
    let (started, starts) = channel();
    let (seen, cancels) = channel();
    let job = Fake {
        started: Some(started),
        spin_until_cancel: true,
        seen_cancel: Some(seen),
        ..Fake::new()
    };
    let b = budget(n, 4);
    let s = Scheduler::new(job, segments(n, |_| false), &b, 1).unwrap();
    assert_eq!(starts.recv().unwrap(), 0);
    drop(s);
    assert_eq!(cancels.recv().unwrap(), (0, true));
    assert!(starts.try_recv().is_err(), "no job started after stop");
    assert_eq!(b.used(Half::Refine), Bytes(0));
}

/// Small xorshift so runs are reproducible without a crate.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap()
    }
}

/// Sums the active estimates itself and watches the budget counter, so a
/// breach is caught from inside the jobs, where it would happen.
struct Watch {
    inner: Fake,
    budget: Budget,
    work_cap: Bytes,
    active: AtomicU64,
    breached: AtomicBool,
}

impl Watch {
    fn check(&self, bytes: Bytes) {
        let active = self.active.fetch_add(bytes.0, Ordering::SeqCst) + bytes.0;
        let used = self.budget.used(Half::Refine);
        if active > self.work_cap.0 || used > self.budget.capacity(Half::Refine) {
            self.breached.store(true, Ordering::SeqCst);
        }
        std::thread::yield_now();
        self.active.fetch_sub(bytes.0, Ordering::SeqCst);
    }
}

impl Job for Watch {
    fn estimate(&self, segment: &Segment) -> Bytes {
        self.inner.estimate(segment)
    }

    fn run(&self, i: usize, segment: &Segment, r: &mut Reservation, c: &Cancel) -> Result<Hunk> {
        assert_eq!(r.bytes(), self.estimate(segment));
        self.check(self.estimate(segment));
        self.inner.run(i, segment, r, c)
    }

    fn too_large(
        &self,
        i: usize,
        segment: &Segment,
        r: &mut Reservation,
        c: &Cancel,
    ) -> Result<Hunk> {
        let limit = r.bytes();
        assert_eq!(limit, self.work_cap);
        assert!(self.estimate(segment) > limit);
        self.check(limit);
        self.inner.too_large(i, segment, r, c)
    }
}

#[test]
fn budget_is_never_exceeded_under_concurrency() {
    let n = 48;
    let runs = 200;
    for seed in 1..=runs {
        let mut rng = Rng(seed);
        let budget = budget(n, 6);
        let half = budget.capacity(Half::Refine);
        let work_cap = Bytes((half.0 - Cache::table_bytes(n).0) / 2);
        let estimates: Vec<u64> = (0..n)
            .map(|_| 1 + rng.below(usize::try_from(work_cap.0 * 5 / 4).unwrap()) as u64)
            .collect();
        let equal = |i: usize| i % 7 == 3;
        let too_large: Vec<usize> = (0..n)
            .filter(|&i| !equal(i) && estimates[i] > work_cap.0)
            .collect();
        let job = Watch {
            inner: Fake {
                estimate: Box::new(move |s| Bytes(estimates[id(s)])),
                ..Fake::new()
            },
            budget: budget.clone(),
            work_cap,
            active: AtomicU64::new(0),
            breached: AtomicBool::new(false),
        };
        let s = Scheduler::new(job, segments(n, equal), &budget, 4).unwrap();
        for _ in 0..24 {
            match rng.below(3) {
                0 => {
                    let at = rng.below(n);
                    s.set_visible(lines(at).start..lines(at + 2).end);
                }
                1 => {
                    let _ = s.hunk(rng.below(n));
                }
                _ => {
                    s.wait_hunk(rng.below(n)).unwrap();
                }
            }
        }
        wait_all(&s);
        let job = &s.shared.job;
        assert!(!job.breached.load(Ordering::SeqCst), "seed {seed}");
        let mut seen: Vec<usize> = job
            .inner
            .too_large
            .lock()
            .unwrap()
            .iter()
            .map(|&(i, _)| i)
            .collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen, too_large, "seed {seed}");
        assert_eq!(s.progress().done, s.progress().total, "seed {seed}");
        assert!(budget.used(Half::Refine) <= budget.capacity(Half::Refine));
        drop(s);
        assert_eq!(
            budget.used(Half::Refine),
            Bytes(0),
            "everything released on drop"
        );
    }
}

#[test]
fn visible_segments_include_an_insert_at_the_top() {
    let seg = |a: Range<u64>, b: Range<u64>| Segment {
        a: LineNo(a.start)..LineNo(a.end),
        b: LineNo(b.start)..LineNo(b.end),
        status: SegmentStatus::Differs,
    };
    let segs = vec![seg(0..0, 0..5), seg(0..10, 5..15), seg(10..10, 15..16)];
    let vis = |r: Range<u64>| visible_segments(&segs, LineNo(r.start)..LineNo(r.end));
    assert_eq!(vis(0..1), 0..2, "the insert at the top shows with line 0");
    assert_eq!(vis(0..10), 0..3);
    assert_eq!(vis(1..10), 1..3);
}

/// Runs `f` on its own thread and fails the test if it has not returned
/// within a few seconds, so a hang shows up as a failure.
fn finishes<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> std::thread::Result<T> {
    let (done, finished) = channel();
    let handle = std::thread::spawn(move || {
        let out = panic::catch_unwind(AssertUnwindSafe(f));
        let _ = done.send(());
        out
    });
    finished
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("did not finish: hang");
    handle.join().expect("result is caught")
}

#[test]
fn a_row_table_larger_than_the_cache_block_is_refused_not_spun_on() {
    // 1000 rows of 24 bytes against a 4 KiB Refine half: a cache of size
    // zero would refuse every hunk and wait_hunk would recompute forever.
    let n = 1000;
    let budget = Budget::new(Bytes(8 << 10));
    match Scheduler::new(Fake::new(), segments(n, |_| false), &budget, 1) {
        Err(crate::Error::Budget(_)) => {}
        Err(e) => panic!("unexpected error {e}"),
        Ok(s) => {
            let s = Arc::new(s);
            finishes(move || s.wait_hunk(n - 1)).unwrap().unwrap();
        }
    }
}

#[test]
fn refine_bytes_held_before_construction_do_not_stall_the_workers() {
    let n = 4;
    let budget = budget(n, 4);
    let half = budget.capacity(Half::Refine);
    let _front_end = budget.try_reserve(Half::Refine, Bytes(half.0 / 2)).unwrap();
    // Every estimate is as large as a job may get.
    let job = Fake {
        estimate: Box::new(|_| Bytes(u64::MAX / 2)),
        ..Fake::new()
    };
    let s = Arc::new(Scheduler::new(job, segments(n, |_| false), &budget, 1).unwrap());
    let t = Arc::clone(&s);
    finishes(move || t.wait_hunk(2)).unwrap().unwrap();
    assert!(budget.used(Half::Refine) <= budget.capacity(Half::Refine));
}

// A debug build asserts that no Refine bytes are held outside the
// scheduler, which kills the worker this test needs.
#[cfg(not(debug_assertions))]
#[test]
#[ignore = "release only: run with --release -- --ignored"]
fn asking_again_wakes_a_worker_that_waited_for_outside_bytes() {
    let n = 1;
    let budget = budget(n, 4);
    let job = Fake {
        estimate: Box::new(|_| Bytes(u64::MAX / 2)),
        ..Fake::new()
    };
    let s = Scheduler::new(job, segments(n, |_| false), &budget, 1).unwrap();
    s.set_background(false);
    let held = budget.try_reserve(Half::Refine, Bytes(64)).unwrap();
    assert!(matches!(s.hunk(0), HunkState::Pending));
    // Give the worker time to fail its reservation and wait.
    std::thread::sleep(std::time::Duration::from_millis(100));
    drop(held);
    let began = std::time::Instant::now();
    while matches!(s.hunk(0), HunkState::Pending) {
        assert!(
            began.elapsed() < std::time::Duration::from_secs(5),
            "still pending 5 s after the outside bytes were released"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

// Release only, as above.
#[cfg(not(debug_assertions))]
#[test]
#[ignore = "release only: run with --release -- --ignored"]
fn a_blocked_waiter_gets_its_hunk_once_outside_bytes_are_released() {
    let n = 1;
    let budget = budget(n, 4);
    let job = Fake {
        estimate: Box::new(|_| Bytes(u64::MAX / 2)),
        ..Fake::new()
    };
    let s = Arc::new(Scheduler::new(job, segments(n, |_| false), &budget, 1).unwrap());
    s.set_background(false);
    let held = budget.try_reserve(Half::Refine, Bytes(64)).unwrap();
    let t = Arc::clone(&s);
    let waiter = std::thread::spawn(move || t.wait_hunk(0).map(|_| ()));
    // Give the worker time to fail its reservation and wait.
    std::thread::sleep(std::time::Duration::from_millis(100));
    drop(held);
    finishes(move || waiter.join().unwrap()).unwrap().unwrap();
}

#[test]
fn a_failing_job_fails_the_waiter_instead_of_hanging() {
    struct Boom;
    impl Job for Boom {
        fn estimate(&self, _: &Segment) -> Bytes {
            Bytes(8)
        }
        fn run(
            &self,
            _: usize,
            segment: &Segment,
            _: &mut Reservation,
            _: &Cancel,
        ) -> Result<Hunk> {
            assert!(id(segment) != 1, "boom");
            if id(segment) == 2 {
                return Err(io::Error::other("disk on fire").into());
            }
            if id(segment) == 3 {
                return Err(REFUSAL.into());
            }
            Ok(script(segment, false))
        }
        fn too_large(
            &self,
            _: usize,
            s: &Segment,
            _: &mut Reservation,
            _: &Cancel,
        ) -> Result<Hunk> {
            Ok(script(s, true))
        }
    }
    const REFUSAL: BudgetExceeded = BudgetExceeded {
        half: Half::Refine,
        requested: Bytes(100),
        available: Bytes(10),
    };
    let n = 5;
    let s = Arc::new(Scheduler::new(Boom, segments(n, |_| false), &budget(n, 4), 1).unwrap());
    let t = Arc::clone(&s);
    let err = finishes(move || t.wait_hunk(1)).unwrap().unwrap_err();
    assert!(err.to_string().contains("panicked"), "{err}");
    let t = Arc::clone(&s);
    let err = finishes(move || t.wait_hunk(2)).unwrap().unwrap_err();
    assert!(err.to_string().contains("disk on fire"), "{err}");
    assert!(err.to_string().contains("segment 2"), "{err}");
    for _ in 0..2 {
        let t = Arc::clone(&s);
        let err = finishes(move || t.wait_hunk(3)).unwrap().unwrap_err();
        assert!(
            matches!(err, crate::Error::Budget(b) if b == REFUSAL),
            "a budget refusal keeps its type for every waiter: {err}"
        );
    }
    let t = Arc::clone(&s);
    finishes(move || t.wait_hunk(4))
        .unwrap()
        .expect("the worker survived");
}

#[test]
fn try_hunk_reports_a_failed_job_where_hunk_would_panic() {
    struct Refused;
    impl Job for Refused {
        fn estimate(&self, _: &Segment) -> Bytes {
            Bytes(8)
        }
        fn run(&self, _: usize, _: &Segment, _: &mut Reservation, _: &Cancel) -> Result<Hunk> {
            Err(REFUSAL.into())
        }
        fn too_large(
            &self,
            _: usize,
            s: &Segment,
            _: &mut Reservation,
            _: &Cancel,
        ) -> Result<Hunk> {
            Ok(script(s, true))
        }
    }
    const REFUSAL: BudgetExceeded = BudgetExceeded {
        half: Half::Refine,
        requested: Bytes(100),
        available: Bytes(10),
    };
    let s = Scheduler::new(Refused, segments(2, |i| i == 0), &budget(2, 4), 1).unwrap();
    assert!(matches!(s.try_hunk(0), Ok(HunkState::Ready(_))));
    assert!(s.wait_hunk(1).is_err());
    let err = s.try_hunk(1).unwrap_err();
    assert!(
        matches!(err, crate::Error::Budget(b) if b == REFUSAL),
        "{err}"
    );
}

#[test]
fn without_background_only_the_window_and_requests_are_refined() {
    let n = 12;
    let (started, starts) = channel();
    let (open, gate) = channel();
    let job = Fake {
        started: Some(started),
        gate: Some(Mutex::new(gate)),
        ..Fake::new()
    };
    let s = Scheduler::new(job, segments(n, |_| false), &budget(n, 64), 1).unwrap();
    // Background is on until the switch, so hold the worker inside its
    // first job until then; otherwise it may take more segments first.
    assert_eq!(starts.recv().unwrap(), 0);
    s.set_background(false);
    for _ in 0..n {
        open.send(()).unwrap();
    }
    s.set_visible(lines(6));
    let mut got: Vec<usize> = (0..9).map(|_| starts.recv().unwrap()).collect();
    got.sort_unstable();
    assert_eq!(got, [2, 3, 4, 5, 6, 7, 8, 9, 10]);
    s.wait_hunk(11).unwrap();
    assert!(
        starts
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_ok_and(|i| i == 11)
    );
    assert!(
        starts
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_err()
    );
}
