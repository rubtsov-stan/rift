//! The counting allocator on a few KiB: small enough for Miri, which runs
//! this binary to check the allocator's `unsafe` forwarding to `System`.
//!
//! Alone in its binary: the counters are process-wide.

use rift_core::alloc::{self, CountingAlloc};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const N: usize = 4096;
/// The test harness allocates a little between reads.
const SLACK: u64 = 2048;

#[test]
fn alloc_zeroed_realloc_and_free_move_the_counters() {
    let base = alloc::current().0;
    alloc::reset_peak();

    let mut v = vec![0u8; N];
    assert!(
        v.iter().all(|&b| b == 0),
        "alloc_zeroed returns zeroed memory"
    );
    assert!(alloc::current().0 >= base + N as u64 - SLACK);

    v.resize(2 * N, 7);
    assert_eq!(v[2 * N - 1], 7);
    assert!(alloc::current().0 >= base + 2 * N as u64 - SLACK);

    v.truncate(N / 2);
    v.shrink_to_fit();
    assert!(alloc::current().0 < base + N as u64);

    drop(v);
    assert!(alloc::current().0 < base + SLACK);
    assert!(
        alloc::peak().0 >= base + 2 * N as u64 - SLACK,
        "the peak survives the free"
    );
}
