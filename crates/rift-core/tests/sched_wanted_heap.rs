//! Asking for every segment's hunk does not grow the heap outside the
//! budget.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters.

use std::sync::Mutex;
use std::sync::mpsc::{Receiver, channel};

use rift_core::alloc::{self, CountingAlloc};
use rift_core::sched::{Cancel, Job, Scheduler};
use rift_core::{Budget, Bytes, Edit, Hunk, LineNo, Reservation, Segment, SegmentStatus};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Blocks every job until the sender is dropped, so requests pile up.
struct Gated(Mutex<Receiver<()>>);

impl Job for Gated {
    fn estimate(&self, _: &Segment) -> Bytes {
        Bytes(64)
    }

    fn run(
        &self,
        _: usize,
        s: &Segment,
        _: &mut Reservation,
        _: &Cancel,
    ) -> rift_core::Result<Hunk> {
        let _ = self.0.lock().unwrap().recv();
        Ok(Hunk {
            edits: vec![
                Edit::Delete {
                    a: s.a.start,
                    len: s.len_a(),
                },
                Edit::Insert {
                    b: s.b.start,
                    len: s.len_b(),
                },
            ],
            approx: None,
        })
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

#[test]
fn two_million_requests_add_no_heap() {
    let n = 2_000_000;
    let segments: Vec<Segment> = (0..n)
        .map(|i| Segment {
            a: LineNo(i)..LineNo(i + 1),
            b: LineNo(i)..LineNo(i + 1),
            status: SegmentStatus::Differs,
        })
        .collect();
    let budget = Budget::new(Bytes::mib(256));
    let (tx, rx) = channel::<()>();
    let s = Scheduler::new(Gated(Mutex::new(rx)), segments, &budget, 1).unwrap();
    s.set_background(false);
    let before = alloc::current().0;
    for i in 0..n {
        let _ = s.try_hunk(usize::try_from(i).unwrap()).unwrap();
    }
    let grown = alloc::current().0.saturating_sub(before);
    eprintln!("heap grew by {grown} bytes for {n} requests");
    drop(tx);
    // A distinct request used to add a set entry, about 20 bytes each.
    assert!(
        grown < 1 << 16,
        "heap grew by {grown} bytes outside the budget"
    );
}
