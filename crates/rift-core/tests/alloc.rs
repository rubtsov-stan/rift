//! Counting allocator checks.
//!
//! The counters are process-wide, so this binary holds nothing else: any
//! other test allocating on a parallel thread would move them underneath
//! the assertions. The two tests still take a lock against each other.

use std::sync::Mutex;

use rift_core::alloc::{self, CountingAlloc};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

static SERIAL: Mutex<()> = Mutex::new(());

const BIG: usize = 64 << 20;
const BIG_U64: u64 = BIG as u64;
/// The test harness itself allocates a little between reads.
const SLACK: u64 = 1 << 20;

#[test]
fn counters_follow_a_large_allocation() {
    let _guard = SERIAL.lock().unwrap();
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

#[test]
fn realloc_adjusts_by_the_difference() {
    let _guard = SERIAL.lock().unwrap();
    let mut v: Vec<u8> = Vec::with_capacity(BIG);
    let base = alloc::current().0;
    v.reserve_exact(2 * BIG);
    assert!(alloc::current().0 >= base + BIG_U64 - SLACK);
    v.shrink_to(BIG);
    assert!(alloc::current().0 < base + SLACK);
}
