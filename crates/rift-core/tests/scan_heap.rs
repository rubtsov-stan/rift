//! A scan of a small file costs little heap, whatever the budget.
//!
//! Alone in its binary because it reads the process-wide allocator counters.

use rift_core::alloc::{self, CountingAlloc};
use rift_core::scan::{ScanConfig, scan};
use rift_core::{Budget, Source};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[test]
fn a_tiny_pair_under_a_huge_budget_peaks_in_kibibytes() {
    let budget = Budget::new("1T".parse().unwrap());
    let (a, b): (&[u8], &[u8]) = (b"x\n", b"y");
    let (cfg_a, cfg_b) = ScanConfig::for_pair(Source::len(a), Source::len(b), &budget);
    let before = alloc::current().0;
    alloc::reset_peak();
    let sa = scan(a, &budget, &cfg_a).unwrap();
    let sb = scan(b, &budget, &cfg_b).unwrap();
    let peak = alloc::peak().0 - before;
    eprintln!("peak heap {peak} bytes");
    assert_eq!(sa.index().line_count() + sb.index().line_count(), 2);
    // Per-worker staging vectors dominate: about 112 KiB, sized by
    // constants, never by the budget.
    assert!(
        peak < 256 << 10,
        "peak heap {peak} bytes for a 3-byte pair under a 1 TiB budget"
    );
    assert!(
        sa.held().0 + sb.held().0 < 4 << 10,
        "held {:?} {:?}",
        sa.held(),
        sb.held()
    );
}
