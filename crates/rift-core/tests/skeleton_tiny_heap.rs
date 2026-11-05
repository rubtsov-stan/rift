//! Mapping a small pair costs little heap, whatever the budget.
//!
//! Alone in its binary because it reads the process-wide allocator counters.

use rift_core::Budget;
use rift_core::alloc::{self, CountingAlloc};
use rift_core::skeleton::build_pair;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[test]
fn a_tiny_pair_under_a_huge_budget_peaks_in_kibibytes() {
    let budget = Budget::new("1T".parse().unwrap());
    let (a, b): (&[u8], &[u8]) = (b"x\nsame\n", b"y\nsame\n");
    let before = alloc::current().0;
    alloc::reset_peak();
    let (_, _, map) = build_pair(a, b, &budget).unwrap();
    let peak = alloc::peak().0 - before;
    eprintln!("peak heap {peak} bytes");
    assert_eq!(map.segments().len(), 2);
    // The scan's staging vectors dominate, about 112 KiB.
    assert!(
        peak < 256 << 10,
        "peak heap {peak} bytes for a 14-byte pair under a 1 TiB budget"
    );
}
