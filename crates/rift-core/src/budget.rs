//! Byte-level memory budget split into two halves.
//!
//! `skeleton + index <= B/2` and `active refinements + hunk cache <= B/2`.
//! Every allocation on the data path reserves its bytes from one half before
//! allocating and releases them by dropping the [`Reservation`].

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::types::Bytes;

/// Which half of the budget pays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Half {
    /// Samples, anchor table, sparse index.
    Skeleton,
    /// Active segment refinements and the hunk cache.
    Refine,
}

impl std::fmt::Display for Half {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Skeleton => "skeleton",
            Self::Refine => "refine",
        })
    }
}

/// A reservation did not fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{half} budget exceeded: requested {requested} bytes, {available} available")]
pub struct BudgetExceeded {
    /// The half that was asked.
    pub half: Half,
    /// Bytes requested.
    pub requested: Bytes,
    /// Bytes free in that half at the time.
    pub available: Bytes,
}

#[derive(Debug)]
struct Inner {
    cap: u64,
    used: [AtomicU64; 2],
}

/// Shared handle to the budget. Cloning shares the same counters.
#[derive(Debug, Clone)]
pub struct Budget(Arc<Inner>);

impl Budget {
    /// A budget of `total` bytes, half per [`Half`].
    #[must_use]
    pub fn new(total: Bytes) -> Self {
        Budget(Arc::new(Inner {
            cap: total.0 / 2,
            used: [AtomicU64::new(0), AtomicU64::new(0)],
        }))
    }

    /// Capacity of one half; both halves are the same size.
    #[must_use]
    pub fn capacity(&self, _half: Half) -> Bytes {
        Bytes(self.0.cap)
    }

    /// Bytes currently reserved in `half`.
    #[must_use]
    pub fn used(&self, half: Half) -> Bytes {
        Bytes(self.0.used[half as usize].load(Ordering::Relaxed))
    }

    /// Bytes still free in `half`.
    #[must_use]
    pub fn available(&self, half: Half) -> Bytes {
        Bytes(self.0.cap.saturating_sub(self.used(half).0))
    }

    /// Reserves `bytes` from `half`.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetExceeded`] when the half cannot hold the request; the
    /// counters are left unchanged.
    pub fn try_reserve(&self, half: Half, bytes: Bytes) -> Result<Reservation, BudgetExceeded> {
        self.0.add(half, bytes.0)?;
        Ok(Reservation {
            budget: self.clone(),
            half,
            bytes: bytes.0,
        })
    }
}

impl Inner {
    fn add(&self, half: Half, n: u64) -> Result<(), BudgetExceeded> {
        // Relaxed is enough: the counter guards no memory, only a number,
        // and a reservation's bytes are handed to the thread that holds it.
        // A CAS loop rather than add-then-undo, so the counter never reads
        // above the capacity, not even transiently.
        let counter = &self.used[half as usize];
        let mut used = counter.load(Ordering::Relaxed);
        loop {
            if used.checked_add(n).is_none_or(|total| total > self.cap) {
                return Err(BudgetExceeded {
                    half,
                    requested: Bytes(n),
                    available: Bytes(self.cap.saturating_sub(used)),
                });
            }
            match counter.compare_exchange_weak(
                used,
                used + n,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(()),
                Err(actual) => used = actual,
            }
        }
    }
}

/// Bytes held in one half of the budget, released on drop.
#[derive(Debug)]
pub struct Reservation {
    budget: Budget,
    half: Half,
    bytes: u64,
}

impl Reservation {
    /// Bytes held.
    #[must_use]
    pub fn bytes(&self) -> Bytes {
        Bytes(self.bytes)
    }

    /// The half this reservation is charged to.
    #[must_use]
    pub fn half(&self) -> Half {
        self.half
    }

    /// Adds `extra` bytes to this reservation.
    ///
    /// # Errors
    ///
    /// Returns [`BudgetExceeded`] when the half cannot hold the extra bytes;
    /// the reservation keeps its previous size.
    pub fn grow(&mut self, extra: Bytes) -> Result<(), BudgetExceeded> {
        self.budget.0.add(self.half, extra.0)?;
        self.bytes += extra.0;
        Ok(())
    }

    /// Gives back `n` bytes, or everything if `n` is larger than held.
    pub fn shrink(&mut self, n: Bytes) {
        let n = n.0.min(self.bytes);
        self.budget.0.used[self.half as usize].fetch_sub(n, Ordering::Relaxed);
        self.bytes -= n;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.shrink(Bytes(self.bytes));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves_are_independent_and_released_on_drop() {
        let b = Budget::new(Bytes(200));
        assert_eq!(b.capacity(Half::Skeleton), Bytes(100));
        let r = b.try_reserve(Half::Skeleton, Bytes(60)).unwrap();
        assert_eq!(b.used(Half::Skeleton), Bytes(60));
        assert_eq!(b.used(Half::Refine), Bytes(0));
        let err = b.try_reserve(Half::Skeleton, Bytes(50)).unwrap_err();
        assert_eq!(err.available, Bytes(40));
        assert_eq!(
            b.used(Half::Skeleton),
            Bytes(60),
            "failed reserve left counters unchanged"
        );
        b.try_reserve(Half::Refine, Bytes(100)).unwrap();
        drop(r);
        assert_eq!(b.used(Half::Skeleton), Bytes(0));
    }

    #[test]
    fn a_request_that_overflows_u64_is_refused() {
        let b = Budget::new(Bytes(100));
        let _held = b.try_reserve(Half::Skeleton, Bytes(10)).unwrap();
        let err = b.try_reserve(Half::Skeleton, Bytes(u64::MAX)).unwrap_err();
        assert_eq!(err.requested, Bytes(u64::MAX));
        assert_eq!(err.available, Bytes(40));
        assert_eq!(b.used(Half::Skeleton), Bytes(10));
    }

    #[test]
    fn grow_and_shrink_track_the_counter() {
        let b = Budget::new(Bytes(100));
        let mut r = b.try_reserve(Half::Refine, Bytes(10)).unwrap();
        r.grow(Bytes(30)).unwrap();
        assert_eq!(r.bytes(), Bytes(40));
        assert!(r.grow(Bytes(11)).is_err());
        assert_eq!(b.used(Half::Refine), Bytes(40));
        r.shrink(Bytes(15));
        assert_eq!(b.used(Half::Refine), Bytes(25));
        r.shrink(Bytes(1000));
        assert_eq!(b.used(Half::Refine), Bytes(0));
    }

    #[test]
    fn concurrent_reservations_never_exceed_capacity() {
        let b = Budget::new(Bytes(2_000));
        std::thread::scope(|s| {
            for _ in 0..8 {
                let b = b.clone();
                s.spawn(move || {
                    for _ in 0..10_000 {
                        if let Ok(r) = b.try_reserve(Half::Refine, Bytes(300)) {
                            assert!(b.used(Half::Refine) <= b.capacity(Half::Refine));
                            drop(r);
                        }
                    }
                });
            }
        });
        assert_eq!(b.used(Half::Refine), Bytes(0));
    }
}
