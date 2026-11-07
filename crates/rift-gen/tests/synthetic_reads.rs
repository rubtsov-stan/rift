//! Reads from a generated source allocate nothing.
//!
//! Alone in its binary and without the libtest harness: the allocator
//! counters are process-wide, and the harness runs its own thread beside
//! the test that may allocate.

use rift_core::alloc::{self, CountingAlloc};
use rift_core::{ByteOffset, Source};
use rift_gen::synthetic::{Side, Synthetic};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

fn main() {
    let a = Synthetic::new(7, 1 << 30, 0.001, Side::A);
    let mut buf = vec![0u8; 1 << 20];
    let base = alloc::current().0;
    alloc::reset_peak();
    for k in 0..64u64 {
        a.read_at(&mut buf, ByteOffset(k * 16_777_259)).unwrap();
    }
    assert_eq!(alloc::peak().0, base, "a read allocated");
}
