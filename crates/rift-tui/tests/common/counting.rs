//! One counting allocator for the tests that measure heap use. A test binary
//! installs it with `#[global_allocator]`.
#![allow(dead_code, missing_docs)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Counting;

/// Calls to `alloc` and `realloc`.
pub static ALLOCS: AtomicUsize = AtomicUsize::new(0);
/// Live heap bytes, and the highest value seen since the last reset.
pub static CURRENT: AtomicUsize = AtomicUsize::new(0);
pub static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(by: usize) {
    ALLOCS.fetch_add(1, Ordering::Relaxed);
    let now = CURRENT.fetch_add(by, Ordering::Relaxed) + by;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

// SAFETY: every call is forwarded unchanged to `System`, which upholds the
// `GlobalAlloc` contract; the counters have no effect on the memory handed out.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        grow(l.size());
        // SAFETY: same layout as the caller passed.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        CURRENT.fetch_sub(l.size(), Ordering::Relaxed);
        // SAFETY: `p` came from `System.alloc` with layout `l`.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        CURRENT.fetch_sub(l.size(), Ordering::Relaxed);
        grow(n);
        // SAFETY: `p` came from `System.alloc` with layout `l`.
        unsafe { System.realloc(p, l, n) }
    }
}
