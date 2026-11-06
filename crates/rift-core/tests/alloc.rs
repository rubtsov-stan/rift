//! Counting allocator checks.
//!
//! The counters are process-wide, so this binary holds nothing else: any
//! other test allocating on a parallel thread would move them underneath
//! the assertions.

use rift_core::alloc::{self, CountingAlloc};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const BIG: usize = 64 << 20;
const BIG_U64: u64 = BIG as u64;
/// The test harness itself allocates a little between reads.
const SLACK: u64 = 1 << 20;

#[test]
fn counters_follow_a_large_allocation() {
    let before = alloc::current().0;
    alloc::reset_peak();
    let v = vec![1u8; BIG];
    assert!(alloc::current().0 >= before + BIG_U64 - SLACK);
    assert!(alloc::peak().0 >= before + BIG_U64 - SLACK);
    drop(v);
    assert!(alloc::current().0 < before + SLACK);
    assert!(
        alloc::peak().0 >= before + BIG_U64 - SLACK,
        "peak survives the free"
    );
}
