//! Counting global allocator: current and peak heap in bytes.
//!
//! The only `unsafe` in the crate. Installed by the binary with
//! `#[global_allocator]`; tests read the peak to check it stays under
//! `--mem`. The counters are process-wide, so a test that asserts on them
//! lives in its own integration test binary (see `tests/alloc.rs`).
//!
//! [`StrictAlloc`] is the tests' stricter variant: it also tracks how far
//! the heap runs ahead of what a [`Budget`] has reserved, so an allocation
//! no reservation pays for shows however much of `--mem` is spare.

use std::alloc::{GlobalAlloc, Layout, System};
use std::ptr;
use std::sync::atomic::{AtomicI64, AtomicPtr, AtomicUsize, Ordering};

use crate::budget::{Budget, Half};
use crate::types::Bytes;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
/// The budget [`StrictAlloc`] compares the heap with; null for none.
static WATCHED: AtomicPtr<Budget> = AtomicPtr::new(ptr::null_mut());
/// Heap less the watched budget's reservations: when [`watch`] began, and
/// the most since.
static BASE: AtomicI64 = AtomicI64::new(0);
static EXCESS: AtomicI64 = AtomicI64::new(0);

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

/// Sets the peak to the current value, and returns that value: the
/// baseline to measure the peak from. Reading [`current`] first instead
/// lets another thread free in between, which leaves the peak below it.
pub fn reset_peak() -> Bytes {
    let now = CURRENT.load(Ordering::Relaxed);
    PEAK.store(now, Ordering::Relaxed);
    // An allocation between the load and the store raised the peak, and
    // the store took it back down.
    PEAK.fetch_max(CURRENT.load(Ordering::Relaxed), Ordering::Relaxed);
    Bytes(now as u64)
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

/// [`CountingAlloc`] that, on every allocation, also compares the heap
/// with the bytes the [`watch`]ed budget holds in both halves.
#[derive(Debug, Default, Clone, Copy)]
pub struct StrictAlloc;

/// Starts comparing the heap with `budget`'s reservations, from what the
/// heap holds beyond them now.
///
/// `'static` because a thread may still read the budget while the next
/// watch begins; tests leak it.
pub fn watch(budget: &'static Budget) {
    WATCHED.store(ptr::from_ref(budget).cast_mut(), Ordering::SeqCst);
    let now = beyond(budget);
    BASE.store(now, Ordering::SeqCst);
    EXCESS.store(now, Ordering::SeqCst);
}

/// The most bytes the heap has held beyond the watched budget's
/// reservations since [`watch`], less what it held beyond them then. Above
/// 0, something allocated what no reservation pays for.
#[must_use]
pub fn excess() -> i64 {
    EXCESS.load(Ordering::SeqCst) - BASE.load(Ordering::SeqCst)
}

fn beyond(budget: &Budget) -> i64 {
    let reserved = budget.used(Half::Skeleton).0 + budget.used(Half::Refine).0;
    let current = CURRENT.load(Ordering::Relaxed) as u64;
    current.cast_signed() - reserved.cast_signed()
}

fn check() {
    let p = WATCHED.load(Ordering::Acquire);
    if !p.is_null() {
        // SAFETY: `watch` stores only `'static` references.
        let budget = unsafe { &*p };
        let now = beyond(budget);
        // Load first: most allocations set no new maximum.
        if now > EXCESS.load(Ordering::Relaxed) {
            EXCESS.fetch_max(now, Ordering::Relaxed);
        }
    }
}

// SAFETY: every method forwards to `CountingAlloc` unchanged and only reads
// counters after it, so the allocator contract is inherited from it.
unsafe impl GlobalAlloc for StrictAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded with the caller's guarantees.
        let p = unsafe { CountingAlloc.alloc(layout) };
        check();
        p
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded with the caller's guarantees.
        let p = unsafe { CountingAlloc.alloc_zeroed(layout) };
        check();
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded with the caller's guarantees.
        unsafe { CountingAlloc.dealloc(ptr, layout) };
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded with the caller's guarantees.
        let p = unsafe { CountingAlloc.realloc(ptr, layout, new_size) };
        check();
        p
    }
}
