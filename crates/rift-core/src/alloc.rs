//! Counting global allocator: current and peak heap in bytes.
//!
//! The only `unsafe` in the crate. Installed by the binary with
//! `#[global_allocator]`; tests read the peak to check it stays under
//! `--mem`. The counters are process-wide, so a test that asserts on them
//! lives in its own integration test binary (see `tests/alloc.rs`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::types::Bytes;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// Wrapper over the system allocator that keeps the two counters.
#[derive(Debug, Default, Clone, Copy)]
pub struct CountingAlloc;

/// Bytes allocated right now.
#[must_use]
pub fn current() -> Bytes {
    Bytes(CURRENT.load(Ordering::Relaxed) as u64)
}

/// Highest value `current()` has reached since start or the last
/// [`reset_peak`].
#[must_use]
pub fn peak() -> Bytes {
    Bytes(PEAK.load(Ordering::Relaxed) as u64)
}

/// Sets the peak to the current value.
pub fn reset_peak() {
    PEAK.store(CURRENT.load(Ordering::Relaxed), Ordering::Relaxed);
}

#[inline]
fn add(n: usize) {
    // Relaxed: the counters are statistics, nothing is published through them.
    let now = CURRENT.fetch_add(n, Ordering::Relaxed) + n;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

#[inline]
fn sub(n: usize) {
    CURRENT.fetch_sub(n, Ordering::Relaxed);
}

// SAFETY: every method forwards to `System` unchanged and only adjusts
// counters, so the allocator contract (no unwinding, layouts round-trip,
// memory stays valid until deallocated) is inherited from `System`.
unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: same layout the caller guaranteed to be non-zero-sized.
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            add(layout.size());
        }
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: as for `alloc`.
        let p = unsafe { System.alloc_zeroed(layout) };
        if !p.is_null() {
            add(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr` came from this allocator with this layout, by contract.
        unsafe { System.dealloc(ptr, layout) };
        sub(layout.size());
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: `ptr` and `layout` match a live allocation, by contract.
        let p = unsafe { System.realloc(ptr, layout, new_size) };
        if !p.is_null() {
            if new_size >= layout.size() {
                add(new_size - layout.size());
            } else {
                sub(layout.size() - new_size);
            }
        }
        p
    }
}
