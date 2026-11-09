//! Resampling a segment of one repeated line stays within the budget.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters.

use std::sync::Arc;

use rift_core::alloc::{self, CountingAlloc};
use rift_core::diff::{Config, Diff};
use rift_core::{Budget, Bytes, DiffView, HunkState};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[test]
fn a_repeated_line_in_every_stratum_keeps_the_samples_at_their_cap() {
    // The hash of this line has 26 trailing zero bits, so it is in every
    // stratum a resample tries, and thinning keeps all its copies.
    const LINE: &[u8] = b"row 63353956\n";
    let n = 300_000;
    let a: Arc<[u8]> = LINE.repeat(n).into();
    let mut b = LINE.repeat(n);
    let mid = b.len() / 2 / LINE.len() * LINE.len();
    b[mid..mid + LINE.len()].copy_from_slice(b"row 00000000\n");
    let b: Arc<[u8]> = b.into();
    let mem = Bytes::mib(12);
    let budget = Budget::new(mem);
    let base = alloc::current().0;
    alloc::reset_peak();
    let diff = Diff::with_config(a, b, &budget, 1, Config { moves: false }).unwrap();
    for i in 0..diff.segments().len() {
        while matches!(diff.try_hunk(i).unwrap(), HunkState::Pending) {
            std::thread::yield_now();
        }
    }
    drop(diff);
    let peak = alloc::peak().0 - base;
    eprintln!("peak heap {peak} bytes under --mem {}", mem.0);
    // Thread names and the scheduler's per-thread bookkeeping are not
    // charged; they take a few KiB.
    assert!(
        peak <= mem.0 + (64 << 10),
        "peak heap {peak} bytes exceeds the {} byte budget",
        mem.0
    );
}
