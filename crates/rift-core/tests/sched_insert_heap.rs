//! A finished hunk stays charged until the cache holds it.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters.

use rift_core::alloc::{self, CountingAlloc};
use rift_core::sched::{Cancel, HUNK_OVERHEAD, Job, Scheduler};
use rift_core::{
    Budget, Bytes, DiffView, Edit, Half, Hunk, LineNo, Reservation, Segment, SegmentStatus,
};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const EDIT: u64 = size_of::<Edit>() as u64;

/// Two small segments, then one too large, over and over. A small job
/// reserves half the work share and a large one all of it; each returns
/// a hunk as large as its reservation.
struct Filler {
    small: Bytes,
}

impl Job for Filler {
    fn estimate(&self, s: &Segment) -> Bytes {
        if s.a.start.0 % 3 == 2 {
            Bytes(u64::MAX / 2)
        } else {
            self.small
        }
    }

    fn run(
        &self,
        _: usize,
        s: &Segment,
        r: &mut Reservation,
        _: &Cancel,
    ) -> rift_core::Result<Hunk> {
        Ok(hunk(s, r.bytes()))
    }

    fn too_large(
        &self,
        i: usize,
        s: &Segment,
        r: &mut Reservation,
        c: &Cancel,
    ) -> rift_core::Result<Hunk> {
        self.run(i, s, r, c)
    }
}

fn hunk(s: &Segment, bytes: Bytes) -> Hunk {
    let n = usize::try_from((bytes.0 - HUNK_OVERHEAD.0) / EDIT).unwrap();
    let mut edits = Vec::with_capacity(n);
    edits.resize(
        n,
        Edit::Delete {
            a: s.a.start,
            len: 1,
        },
    );
    Hunk {
        edits,
        approx: None,
    }
}

#[test]
fn finished_hunks_and_new_jobs_fit_the_refine_half() {
    let n = 3000;
    let segments: Vec<Segment> = (0..n)
        .map(|i| Segment {
            a: LineNo(i)..LineNo(i + 1),
            b: LineNo(i)..LineNo(i + 1),
            status: SegmentStatus::Differs,
        })
        .collect();
    let budget = Budget::new(Bytes::mib(2));
    let half = budget.capacity(Half::Refine).0;
    let base = alloc::current().0;
    alloc::reset_peak();
    // The work share is what Scheduler::new leaves: half of the rest of
    // the Refine half after the row table.
    let table = 24 * n + 8 * n.div_ceil(4096);
    let small = Bytes((half - table) / 2 / 2);
    let s = Scheduler::new(Filler { small }, segments, &budget, 2).unwrap();
    assert_eq!(s.work_cap().0, 2 * small.0);
    // Background work refines every segment. Polling progress holds no
    // hunk: one held through `wait_hunk` stays allocated after eviction,
    // outside the budget by design.
    while s.progress().done < n {
        std::thread::yield_now();
    }
    let peak = alloc::peak().0 - base;
    drop(s);
    eprintln!("peak heap {peak} bytes, Refine half {half}");
    // The worker threads' own bookkeeping is not charged: a few KiB.
    assert!(
        peak <= half + (16 << 10),
        "peak heap {peak} bytes exceeds the {half} byte Refine half"
    );
}
