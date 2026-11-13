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
///
/// `available` is what the half had free when it refused, so the half held
/// its capacity minus `available`, and a half of at least that plus
/// `requested` is needed to get past this point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{half} budget exceeded: requested {requested} bytes, {available} available")]
pub struct BudgetExceeded {
    /// The half that was asked.
    pub half: Half,
    /// Bytes requested.
    pub requested: Bytes,
    /// Bytes free in that half at the time, counting bytes the caller
    /// could have released first.
    pub available: Bytes,
}

impl BudgetExceeded {
    /// A total budget to run again with, for a run that had `mem` and was
    /// refused this way: twice what the half held plus the request, so
    /// the half alone gets that much. `None` when the refusal does not
    /// exceed what `mem` gives the half, or the figure overflows.
    ///
    /// Not a lower bound, since what a half holds depends on `mem` and a
    /// smaller budget can take another path, nor always enough, since a
    /// later stage may need more. The CLI's `rerun with --mem` and the
    /// protocol's `suggested_mem` both come from here.
    #[must_use]
    pub fn mem_advice(&self, mem: Bytes) -> Option<Bytes> {
        let cap = Budget::new(mem).capacity(self.half).0;
        let held = cap.saturating_sub(self.available.0);
        let need = held.checked_add(self.requested.0)?.checked_mul(2)?;
        (need > mem.0).then_some(Bytes(need))
    }
}

/// The most threads a scan, the skeleton or the refinement uses. What a
/// run is allowed is sized from `--mem` up to this, never from the
/// machine, so the output is the same on any machine; the cores only cap
/// how many of the allowed threads start.
pub const MAX_THREADS: usize = 8;

/// The threads rayon's global pool starts, by rayon's own rule
/// (`ThreadPoolBuilder::get_num_threads` in rayon-core): a positive
/// `RAYON_NUM_THREADS`, else, unless that is 0, a positive
/// `RAYON_RS_NUM_CPUS`, else the cores. Asking rayon would start the
/// pool, whose heap a small budget cannot hold.
#[must_use]
pub fn pool_threads() -> usize {
    let var = |name| std::env::var(name).ok()?.parse::<usize>().ok();
    let cores = || std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    match var("RAYON_NUM_THREADS") {
        Some(n @ 1..) => n,
        Some(0) => cores(),
        None => var("RAYON_RS_NUM_CPUS")
            .filter(|&n| n > 0)
            .unwrap_or_else(cores),
    }
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
    fn a_refusal_advises_twice_what_its_half_needed() {
        let refusal = |half, requested, available| BudgetExceeded {
            half,
            requested: Bytes(requested),
            available: Bytes(available),
        };
        let mem = Bytes::mib(7);
        // 3.5 MiB per half, 0.5 MiB of it free: 3 MiB held, 5 MiB needed.
        let r = refusal(Half::Refine, 2 << 20, 512 << 10);
        assert_eq!(r.mem_advice(mem), Some(Bytes(10 << 20)));
        // A refusal that does not exceed its half proves nothing about mem.
        assert_eq!(refusal(Half::Skeleton, 10, 20).mem_advice(mem), None);
        assert_eq!(refusal(Half::Refine, u64::MAX, 0).mem_advice(mem), None);
    }

    #[test]
    fn pool_threads_counts_what_rayon_starts() {
        assert_eq!(pool_threads(), rayon::current_num_threads());
    }

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
