//! The counting allocator tracks a reallocation by its difference.
//!
//! Alone in its binary: the counters are process-wide.

use rift_core::alloc::{self, CountingAlloc};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const BIG: usize = 64 << 20;
const BIG_U64: u64 = BIG as u64;
/// The test harness itself allocates a little between reads.
const SLACK: u64 = 1 << 20;

#[test]
fn realloc_adjusts_by_the_difference() {
    let mut v: Vec<u8> = Vec::with_capacity(BIG);
    let base = alloc::current().0;
    v.reserve_exact(2 * BIG);
    assert!(alloc::current().0 >= base + BIG_U64 - SLACK);
    v.shrink_to(BIG);
    assert!(alloc::current().0 < base + SLACK);
}
