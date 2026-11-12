//! Passes over both files, each owning one range of key hashes.
//!
//! A pass reads A and B side by side, at the same relative position, and
//! keeps only the records whose key hash falls in its range. A record
//! waits, with its bytes, until the same key turns up in the other file;
//! then the two are compared byte by byte and only a 31-bit fingerprint
//! of the key hash stays, to catch a third occurrence. Files in roughly
//! the same order keep few records waiting, so a pass costs about 5 bytes
//! per key of its range.
//!
//! The range is split between shards, one per thread, by bits of the key
//! hash the table does not use; each shard owns a table, its waiting
//! records and its duplicates, and they take the records of a window in
//! parallel. Each shard grows its lists from its own share of what the
//! refine half had free when the round began, so whether a shard runs out
//! depends on its records alone, never on which thread grew first, and a
//! pass ends at the same place on every run. When a shard's table or
//! waiting records outgrow the budget,
//! the pass starts over on a narrower range, sized from how far it had
//! read: a fingerprint cannot say which keys lie above a new top, so the
//! table cannot be thinned in place. Nothing is ever written to disk.
//!
//! A key whose fingerprint matches a matched key is a suspect: a
//! duplicate, or another key. The next pass, or a last read when there is
//! none, counts the suspects in their file.

use std::fmt;
use std::io;
use std::sync::Mutex;

use rayon::prelude::*;

use super::read::{Filter, Reader, Record, SUSPECTS, Suspect, Tables, grow, reader_bytes, spend};
use super::{ChangeKind, KeyError, KeyedChange, RangedChange, Side};
use crate::budget::{Budget, BudgetExceeded, Half, Reservation};
use crate::error::{Error, Result};
use crate::source::Source;
use crate::types::{Bytes, LineNo};

/// One past the largest key hash.
const END: u64 = 1 << 63;
/// A table slot holding the index of a waiting record, not a fingerprint.
const WAITING: u32 = 1 << 31;
const EMPTY: u32 = 0;
/// Duplicate keys listed in the error.
const FEW: usize = 5;
/// Read window per file, from the refine half.
const WINDOW: u64 = 4 << 20;
/// Most passes a run may take: a budget that would need more is refused,
/// with advice that fits the run within them.
const MAX_PASSES: u64 = 64;
/// Refine-half bytes a shard should have to itself.
const SHARD_BYTES: u64 = 1 << 20;
/// Most shards and most chunks per window. Every shard looks at every
/// record of a round to find its own, so past a few shards the scan costs
/// more than the split saves; a constant, so the shards stay the same on
/// any machine.
const MAX_SHARDS: usize = 8;
/// Refine-half bytes held for each thread of the pool that runs the
/// shards: its queues and thread record, about 8 KiB per thread on macOS,
/// held twice over.
const POOL_PER_THREAD: u64 = 16 << 10;

/// What the comparison cost.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Report {
    /// Passes over both files.
    pub passes: u32,
    /// Records in A.
    pub records_a: u64,
    /// Records in B.
    pub records_b: u64,
    /// Changes reported.
    pub changes: u64,
}

/// Compares the records of `a` and `b` by the key `spec` names, under
/// `budget`, and hands each difference to `on_change`.
///
/// Each pass prints its changes ordered by line (the line in A, or in B
/// for an added record). Most inputs that fit the budget take one pass.
/// The read windows and the waiting records come from the refine half,
/// the hash tables from the skeleton half. A duplicate key can surface a
/// pass after the changes of its pass were handed out.
///
/// Time is O(passes × (|A| + |B|)), spread over the rayon threads; a pass
/// holds about 5 bytes per key of its range plus the records still
/// waiting for their match.
///
/// # Errors
///
/// [`KeyError`] for duplicate or missing keys or a record longer than
/// key mode holds, a budget error when not even a single key or record
/// fits, an I/O error from a source or from `on_change`. A budget error
/// asks for at least twice the half it ran out of, and its numbers are
/// the same on every run.
pub fn compare<A, B>(
    a: &A,
    b: &B,
    spec: &super::KeySpec,
    budget: &Budget,
    on_change: impl FnMut(&KeyedChange) -> io::Result<()>,
) -> Result<Report>
where
    A: Source + ?Sized,
    B: Source + ?Sized,
{
    run::<A, B, ()>(a, b, spec, budget, MAX_PASSES, on_change).map_err(|e| doubled(e, budget))
}

/// [`compare`], handing out each change with the byte ranges of its
/// records, so a caller can read them back.
///
/// Each record that waits for its match, and each changed key until its
/// pass ends, holds 24 more bytes of the refine half, so a tight budget
/// can take more passes than [`compare`] does.
///
/// # Errors
///
/// As [`compare`].
pub fn compare_with_ranges<A, B>(
    a: &A,
    b: &B,
    spec: &super::KeySpec,
    budget: &Budget,
    on_change: impl FnMut(&RangedChange) -> io::Result<()>,
) -> Result<Report>
where
    A: Source + ?Sized,
    B: Source + ?Sized,
{
    run::<A, B, Ranges>(a, b, spec, budget, MAX_PASSES, on_change).map_err(|e| doubled(e, budget))
}

/// A budget refusal that asks for a half at least twice the size, built
/// once the run has given back what it held.
///
/// Where a pass ends moves with the budget, so a refusal cannot say what a
/// rerun will need. Doubling gets there in a few reruns and overshoots by
/// less than twice the least budget that works. Read after every thread
/// has stopped, the numbers are the same on every run.
#[cold]
fn doubled(err: Error, budget: &Budget) -> Error {
    let Error::Budget(refusal) = err else {
        return err;
    };
    let cap = budget.capacity(refusal.half).0;
    let asked = cap - refusal.available.0.min(cap) + refusal.requested.0;
    let need = asked.max(2 * cap);
    let available = budget.available(refusal.half);
    Error::Budget(BudgetExceeded {
        half: refusal.half,
        requested: Bytes(need - (cap - available.0.min(cap))),
        available,
    })
}

#[allow(
    clippy::too_many_lines,
    reason = "the pass loop and the three ways a read ends, kept in one place"
)]
fn run<A, B, X: Extra>(
    a: &A,
    b: &B,
    spec: &super::KeySpec,
    budget: &Budget,
    limit: u64,
    mut on_change: impl FnMut(&X::Change) -> io::Result<()>,
) -> Result<Report>
where
    A: Source + ?Sized,
    B: Source + ?Sized,
{
    let (shards, pool) = plan(budget.available(Half::Refine).0);
    let pool = budget.try_reserve(Half::Refine, Bytes(pool))?;
    let workers = workers(&pool)?;
    let lens = [a.len().0, b.len().0];
    let (len_a, len_b) = (lens[0], lens[1]);
    let sizes = Sizes::new(budget.available(Half::Refine).0, shards, lens);
    let mut ra = Reader::new(a, Side::A, spec, budget, sizes.windows[0], sizes.chunks)?;
    let mut rb = Reader::new(b, Side::B, spec, budget, sizes.windows[1], sizes.chunks)?;
    let tables = Mutex::new(Tables::new(budget)?);
    let keys = ra.estimate_lines()? + rb.estimate_lines()?;
    let (mut shards, _list) = Pass::<X>::shards(budget, keys, sizes.chunks)?;
    let mut pending = Pending::new(budget)?;
    let mut report = Report::default();
    let (mut lo, mut width) = (0, END);
    // Below the narrowest range that ran out of waiting room.
    let mut ceiling = END;
    // The shards take their turns on the whole refine half.
    let mut solo = false;
    loop {
        let hi = lo + width.min(END - lo);
        let mut readers = (&mut ra, &mut rb, &tables);
        let read = on(workers.as_ref(), || {
            read_pass(&mut readers, &mut shards, lo, hi, &pending.list, solo)
        });
        let read = read.map_err(|e| sized_for_record(e, &ra, &rb, pool.bytes().0, budget, lens));
        match read? {
            Outcome::Overflow(half, progress, alone) => {
                let w = hi - lo;
                ra.shrink_lists();
                rb.shrink_lists();
                if alone && !solo {
                    // A narrower range cannot help a record that its
                    // shard's share does not hold.
                    solo = true;
                } else {
                    let refused = if w <= 1 {
                        Err(Error::Budget(BudgetExceeded {
                            half,
                            requested: Bytes(1),
                            available: budget.available(half),
                        }))
                    } else {
                        width = narrowed(half, w, progress, fill(&shards), &mut ceiling);
                        // Keys still to come, at least: those of the range
                        // the tables met, and the larger of the two files'
                        // counts of records read so far above the range.
                        let held: usize = shards.iter().map(|s| s.used).sum();
                        let keys = held as u64 + ra.above().max(rb.above());
                        within(
                            &shards,
                            budget,
                            (report.passes, limit),
                            keys,
                            END - lo,
                            ceiling,
                        )
                    };
                    if let Err(e) = refused {
                        let mut readers = (&mut ra, &mut rb, &tables);
                        let dups = settle(
                            &mut shards,
                            &mut pending,
                            &mut readers,
                            workers.as_ref(),
                            solo,
                        );
                        return Err(or_duplicates(e, dups));
                    }
                }
            }
            Outcome::Stop => {
                let lost = shards.iter().map(|s| u64::from(s.lost)).find(|&b| b > 0);
                let mut readers = (&mut ra, &mut rb, &tables);
                let dups = settle(
                    &mut shards,
                    &mut pending,
                    &mut readers,
                    workers.as_ref(),
                    solo,
                )
                .map_err(|e| sized_for_record(e, &ra, &rb, pool.bytes().0, budget, lens))?;
                if let (true, Some(bytes)) = (dups.is_empty(), lost) {
                    // A duplicate went unlisted and no other turned up:
                    // refuse, and the advice gives the list room.
                    return Err(Error::Budget(BudgetExceeded {
                        half: Half::Refine,
                        requested: Bytes(bytes),
                        available: budget.available(Half::Refine),
                    }));
                }
                return Err(if dups.is_empty() {
                    KeyError::Collisions
                } else {
                    KeyError::Duplicate(dups)
                }
                .into());
            }
            Outcome::Done => {
                let dups = collect_dups(&mut shards, pending.duplicates(&ra, &rb));
                if !dups.is_empty() {
                    return Err(KeyError::Duplicate(dups).into());
                }
                if let Some(bytes) = shards.iter().map(|s| u64::from(s.lost)).find(|&b| b > 0) {
                    // A duplicate went unlisted and no other turned up:
                    // refuse, and the advice gives the list room.
                    return Err(Error::Budget(BudgetExceeded {
                        half: Half::Refine,
                        requested: Bytes(bytes),
                        available: budget.available(Half::Refine),
                    }));
                }
                if lo == END {
                    break;
                }
                if report.passes == 0 {
                    (report.records_a, report.records_b) = (ra.records(), rb.records());
                    if let super::KeySpec::Copy(name) = spec
                        && ra.blocks() + rb.blocks() == 0
                        && len_a + len_b > 0
                    {
                        return Err(KeyError::NoColumn {
                            side: Side::A,
                            name: name.clone(),
                        }
                        .into());
                    }
                }
                report.changes += emit(&mut shards, &mut on_change)?;
                report.passes += 1;
                pending.replace(&mut shards)?;
                // Both files were read whole: their records above the
                // range are the keys still to come, counted in each.
                let keys = ra.above().max(rb.above());
                if let Err(e) = within(
                    &shards,
                    budget,
                    (report.passes, limit),
                    keys,
                    END - hi,
                    ceiling,
                ) {
                    let mut readers = (&mut ra, &mut rb, &tables);
                    let dups = settle(
                        &mut shards,
                        &mut pending,
                        &mut readers,
                        workers.as_ref(),
                        solo,
                    );
                    return Err(or_duplicates(e, dups));
                }
                // A range cut short by waiting records may leave the
                // tables half empty; widen the next pass toward 90%.
                let grown = scale(hi - lo, (0.9 / fill(&shards).max(f64::EPSILON)).min(2.0));
                width = width.max(grown.min(ceiling));
                lo = hi;
                if lo == END && pending.list.is_empty() {
                    break;
                }
            }
        }
    }
    Ok(report)
}

/// How the read windows are sized for a refine half with `refine` bytes
/// free after the thread pool's share.
struct Sizes {
    /// The share before it is split in proportion to the files.
    window: u64,
    windows: [usize; 2],
    chunks: usize,
}

impl Sizes {
    fn new(refine: u64, shards: usize, lens: [u64; 2]) -> Self {
        // Windows in proportion to the files keep both at the same
        // relative position.
        let window = (refine / 16).clamp(64, WINDOW);
        let longest = lens[0].max(lens[1]).max(1);
        let win = |len: u64| {
            let w = u128::from(window) * u128::from(len) / u128::from(longest);
            usize::try_from(w).unwrap_or(usize::MAX).max(64)
        };
        Self {
            window,
            windows: [win(lens[0]), win(lens[1])],
            // A chunk per shard.
            chunks: shards,
        }
    }
}

/// Turns a refusal to grow a window for one long record into a refusal
/// that names a refine half under which that record fits.
///
/// Each window starts at its [`Sizes`] share and doubles until the record
/// fits, so at a refine half of `r` bytes the window ends at `s(r)`, the
/// first `window(r) · 2^k` that holds the record. Besides the windows, the
/// half holds the pool, `p(r)`, and lists of the records a window holds
/// or that wait for their match. Those are taken to grow with the window
/// share, `window(r)`, from the `rest` they took at the refusal: they
/// take up to half of what is free each time they grow, and a window of
/// twice the bytes holds twice the records. The half needed is the
/// smallest `r` above the current one with
///
/// `r ≥ p(r) + Σ reader_bytes(s_side(r), chunks(r)) + rest · window(r) / window(now)`
///
/// where `s_side(r)` is the window grown for the record at that side's
/// front, read to its end, or for the longest record it held before. The
/// search steps by 1/512, so the answer is at most that much above the
/// least `r`.
#[cold]
fn sized_for_record<A, B>(
    err: Error,
    ra: &Reader<'_, A>,
    rb: &Reader<'_, B>,
    pool: u64,
    budget: &Budget,
    lens: [u64; 2],
) -> Error
where
    A: Source + ?Sized,
    B: Source + ?Sized,
{
    let Error::Budget(refusal) = err else {
        return err;
    };
    if refusal.half != Half::Refine || !(ra.oversized() || rb.oversized()) {
        return err;
    }
    // Each window must grow to hold the record at its front and, if it
    // grew before, the longest it held so far.
    let cap = budget.capacity(Half::Refine).0;
    let now = Sizes::new(cap.saturating_sub(pool), 1, lens);
    let shares = now.windows;
    let records = match (ra.front_record(), rb.front_record()) {
        (Ok(a), Ok(b)) => [
            a.max(ra.grown(shares[0]).unwrap_or(0)),
            b.max(rb.grown(shares[1]).unwrap_or(0)),
        ],
        (Err(e), _) | (_, Err(e)) => return e,
    };
    let used = cap.saturating_sub(refusal.available.0);
    let held = [ra.held(), rb.held()];
    let rest = used.saturating_sub(pool + held[0] + held[1]);
    let fits = |r: u64| {
        let (n, pool) = plan(r);
        let sizes = Sizes::new(r.saturating_sub(pool), n, lens);
        let mut need = pool;
        for (&share, &record) in sizes.windows.iter().zip(&records) {
            let mut window = share as u64;
            while window < record {
                window *= 2;
            }
            need += reader_bytes(usize::try_from(window).unwrap_or(usize::MAX), sizes.chunks);
        }
        let lists = u128::from(rest) * u128::from(sizes.window) / u128::from(now.window);
        let lists = u64::try_from(lists).unwrap_or(u64::MAX);
        need.saturating_add(lists) <= r
    };
    let mut r = cap + 1;
    while r < 1 << 60 {
        if fits(r) {
            return Error::Budget(BudgetExceeded {
                half: Half::Refine,
                requested: Bytes(r - used),
                available: refusal.available,
            });
        }
        r += (r / 512).max(1 << 10);
    }
    err
}

/// The range a pass that ran out of `half` at `progress` through the files
/// starts over with, `w` wide with its fullest table at `fill`.
///
/// The tables fill as the files are read, so the new range keeps the
/// share they should hold to the end, with a margin. Waiting records do
/// not pile up the same way: they are the ones whose match is still
/// ahead, so a full refine half halves the range, unless the tables ask
/// for less, and caps how far later passes widen it.
fn narrowed(half: Half, w: u64, progress: f64, fill: f64, ceiling: &mut u64) -> u64 {
    let tables = 0.9 * progress / fill.max(f64::EPSILON);
    let keep = match half {
        Half::Skeleton => tables,
        Half::Refine => {
            *ceiling = (*ceiling).min(scale(w, 0.9));
            tables.min(0.5)
        }
    };
    scale(w, keep).clamp(1, w - 1)
}

/// The fewest passes `keys` distinct keys can take, `rest` from the
/// start of their range to the end of the hash range, and the half that
/// bounds them.
///
/// A pass that ends holds every key of its range in its tables, so it
/// covers at most their capacity; after waiting records overflowed, no
/// pass is wider than `ceiling`. The caller counts `keys` among records
/// already read, so both are lower bounds, whatever the keys and however
/// the files are laid out, and depend on the files and the budget alone.
fn passes_left<X: Extra>(
    shards: &[Pass<'_, X>],
    keys: u64,
    rest: u64,
    ceiling: u64,
) -> (u64, Half) {
    let capacity: usize = shards.iter().map(|s| s.limit).sum();
    let by_tables = keys.div_ceil((capacity as u64).max(1));
    let by_width = rest.div_ceil(ceiling.max(1));
    if by_width > by_tables {
        (by_width, Half::Refine)
    } else {
        (by_tables, Half::Skeleton)
    }
}

/// A budget refusal if a run that has taken `done` passes needs more
/// than `limit` in all, by [`passes_left`] for `keys` keys from `rest`
/// before the end of the hash range.
fn within<X: Extra>(
    shards: &[Pass<'_, X>],
    budget: &Budget,
    (done, limit): (u32, u64),
    keys: u64,
    rest: u64,
    ceiling: u64,
) -> Result<()> {
    let left = passes_left(shards, keys, rest, ceiling);
    match too_many_passes(budget, u64::from(done), left, limit) {
        Some(refusal) => Err(Error::Budget(refusal)),
        None => Ok(()),
    }
}

/// A refusal for a run that has taken `done` passes and needs at least
/// `left` more of a `short` half, if that is over `limit`: the half twice
/// as many times larger. A half `k` times larger holds about `k` times
/// the keys per pass, but the count is a lower bound and not every list
/// grows with the half, so this asks for twice that, and [`doubled`] for
/// at least twice the half; a rerun still short is refused again with
/// the same advice, so following it steps up geometrically.
fn too_many_passes(
    budget: &Budget,
    done: u64,
    (left, short): (u64, Half),
    limit: u64,
) -> Option<BudgetExceeded> {
    let passes = done.saturating_add(left);
    if passes <= limit {
        return None;
    }
    let cap = budget.capacity(short).0;
    let need = 2 * u128::from(cap) * u128::from(passes) / u128::from(limit.max(1));
    let available = budget.available(short);
    let held = cap - available.0.min(cap);
    Some(BudgetExceeded {
        half: short,
        requested: Bytes(u64::try_from(need).unwrap_or(u64::MAX).saturating_sub(held)),
        available,
    })
}

/// The fullest shard table, as a share of its limit.
fn fill<X: Extra>(shards: &[Pass<'_, X>]) -> f64 {
    #[allow(clippy::cast_precision_loss, reason = "a ratio")]
    shards
        .iter()
        .map(|s| s.used as f64 / s.limit.max(1) as f64)
        .fold(0.0, f64::max)
}

/// `w` times `factor`, saturating.
fn scale(w: u64, factor: f64) -> u64 {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "an estimate, clamped by the caller"
    )]
    let v = (w as f64 * factor) as u64;
    v
}

/// Shards for a refine half with `refine` bytes free, and the refine-half
/// bytes to hold for the threads that run them.
///
/// The shards follow the budget only, never the thread count, so the
/// passes and the order of their changes are the same on any machine. A
/// pool is held for one thread per shard, the most [`workers`] starts,
/// so what is left of the half does not depend on the pool either. One
/// shard runs on the caller's thread and holds no pool.
fn plan(refine: u64) -> (usize, u64) {
    let shards = usize::try_from(refine / SHARD_BYTES)
        .unwrap_or(usize::MAX)
        .clamp(1, MAX_SHARDS);
    let pool = if shards > 1 {
        POOL_PER_THREAD * shards as u64
    } else {
        0
    };
    (shards, pool)
}

/// The threads that run the shards, as many as the `pool` held for them
/// pays for, at most rayon's pool size; `None` when it pays for none, as
/// for one shard, which runs on the caller's thread. Rayon's global pool
/// is never started: its size follows the machine, and its heap is not
/// in the budget.
///
/// A thread's own working set is the pool's heap for it,
/// [`POOL_PER_THREAD`]. Everything else belongs to a file, a chunk or a
/// shard, whichever thread works on it: the two windows (`Reader::buf`,
/// sized by [`Sizes`]), the record list of each chunk (`Chunk::recs`, as
/// many chunks per window as shards), and each shard's table and lists
/// (`Pass::slots`, `kept`, `arena`, `dups`, `suspects`). Thread stacks
/// are mapped by the system, outside the heap. So the threads number
/// `min(pool size, pool / POOL_PER_THREAD)`, which [`plan`] makes at
/// most one per shard.
fn workers(pool: &Reservation) -> Result<Option<rayon::ThreadPool>> {
    let paid = usize::try_from(pool.bytes().0 / POOL_PER_THREAD).unwrap_or(usize::MAX);
    if paid == 0 {
        return Ok(None);
    }
    rayon::ThreadPoolBuilder::new()
        .num_threads(crate::budget::pool_threads().min(paid))
        .build()
        .map(Some)
        .map_err(|e| Error::Io(io::Error::other(e)))
}

/// Runs `op` on `workers`, or on this thread without them.
fn on<R: Send>(workers: Option<&rayon::ThreadPool>, op: impl FnOnce() -> R + Send) -> R {
    match workers {
        Some(pool) => pool.install(op),
        None => op(),
    }
}

/// How a read of both files ended.
enum Outcome {
    /// Both files were read to the end.
    Done,
    /// A shard ran out of the half named, this far through the files;
    /// `true` if a shard's share could not hold one of its records.
    Overflow(Half, f64, bool),
    /// Enough duplicates, or too many suspects, to stop.
    Stop,
}

type Readers<'r, 's, A, B> = (
    &'r mut Reader<'s, A>,
    &'r mut Reader<'s, B>,
    &'r Mutex<Tables>,
);

/// Reads both files once, the shards taking the records in `lo..hi`, and
/// counts `pending` on the way. An empty range only counts. The shards
/// take their records in parallel, each from its share of the refine
/// half, unless `solo`: then in turn, from all of it.
fn read_pass<A, B, X: Extra>(
    (ra, rb, tables): &mut Readers<'_, '_, A, B>,
    shards: &mut [Pass<'_, X>],
    lo: u64,
    hi: u64,
    pending: &[Suspect],
    solo: bool,
) -> Result<Outcome>
where
    A: Source + ?Sized,
    B: Source + ?Sized,
{
    for s in shards.iter_mut() {
        s.start(lo, hi);
    }
    ra.rewind();
    rb.rewind();
    let filter = Filter::new(lo, hi, pending);
    let total = ra.len() + rb.len();
    let n = shards.len();
    let budget = shards[0].budget;
    // Lists that grow side by side split what is free between them, so
    // where a pass ends depends on the files and the budget, not on which
    // thread grew first. One shard runs everything on one thread.
    let share = |parts: usize| {
        if n == 1 {
            u64::MAX
        } else {
            budget.available(Half::Refine).0 / parts as u64
        }
    };
    loop {
        ra.make_room()?;
        rb.make_room()?;
        let chunks = share(2 * n);
        ra.set_share(chunks);
        rb.set_share(chunks);
        let (ga, gb) = if n == 1 {
            (ra.advance(tables, &filter), rb.advance(tables, &filter))
        } else {
            rayon::join(
                || ra.advance(tables, &filter),
                || rb.advance(tables, &filter),
            )
        };
        #[allow(clippy::cast_precision_loss, reason = "an estimate")]
        let progress = (ra.consumed() + rb.consumed()) as f64 / total.max(1) as f64;
        let (ga, gb) = match both(ga, gb) {
            Ok(g) => g,
            // A window that cannot keep one record or name one table is
            // short of what the shards hold: give it back and narrow the
            // range.
            Err(Error::Budget(_)) if lo < hi => {
                for s in shards.iter_mut() {
                    s.release();
                }
                return Ok(Outcome::Overflow(Half::Refine, progress, false));
            }
            Err(e) => return Err(e),
        };
        if !ga && !gb {
            return Ok(Outcome::Done);
        }
        if lo == hi {
            continue;
        }
        let (ra, rb) = (&**ra, &**rb);
        let credit = if solo { u64::MAX } else { share(n) };
        for s in shards.iter_mut() {
            (s.credit, s.share) = (credit, credit);
        }
        if n == 1 || solo {
            for (k, s) in shards.iter_mut().enumerate() {
                s.round(k, n, ra, rb);
            }
        } else {
            shards
                .par_iter_mut()
                .enumerate()
                .for_each(|(k, s)| s.round(k, n, ra, rb));
        }
        for s in shards.iter_mut() {
            if let Some(e) = s.fault.take() {
                return Err(e);
            }
        }
        let dups: usize = shards.iter().map(|s| s.dups.len()).sum();
        let suspects: usize = shards.iter().map(|s| s.suspects.len()).sum();
        let overflow = shards.iter().find_map(|s| s.overflow);
        // A duplicate found waiting is certain: report it rather than
        // start the pass over.
        if dups >= FEW || suspects >= SUSPECTS || (overflow.is_some() && dups > 0) {
            return Ok(Outcome::Stop);
        }
        if let Some(half) = overflow {
            let alone = shards.iter().any(|s| s.alone);
            return Ok(Outcome::Overflow(half, progress, alone));
        }
    }
}

/// Both readers' results, or the error to act on: an error in the data,
/// such as a record without a key, before a window short of room, which
/// a narrower pass could get past.
fn both(a: Result<bool>, b: Result<bool>) -> Result<(bool, bool)> {
    match (a, b) {
        (Ok(a), Ok(b)) => Ok((a, b)),
        (Err(Error::Budget(_)) | Ok(_), Err(e)) | (Err(e), _) => Err(e),
    }
}

/// The duplicate keys a read that ended early can still name: those the
/// shards found waiting, else those among the suspects of this read, or
/// of the last one if this read has none, counted by one more read in
/// room the records no longer need.
fn settle<A, B, X: Extra>(
    shards: &mut [Pass<'_, X>],
    pending: &mut Pending,
    readers: &mut Readers<'_, '_, A, B>,
    workers: Option<&rayon::ThreadPool>,
    solo: bool,
) -> Result<Vec<Vec<u8>>>
where
    A: Source + ?Sized,
    B: Source + ?Sized,
{
    let dups = collect_dups(shards, Vec::new());
    if !dups.is_empty() {
        return Ok(dups);
    }
    for s in shards.iter_mut() {
        s.release();
    }
    if shards.iter().any(|s| !s.suspects.is_empty()) {
        pending.replace(shards)?;
        for s in shards.iter_mut() {
            s.release();
        }
    }
    if pending.list.is_empty() {
        return Ok(dups);
    }
    let list = &pending.list;
    on(workers, || read_pass(readers, shards, END, END, list, solo))?;
    Ok(pending.duplicates(readers.0, readers.1))
}

/// A data error before a budget refusal: duplicate keys, if `dups` names
/// any, else `refusal`.
fn or_duplicates(refusal: Error, dups: Result<Vec<Vec<u8>>>) -> Error {
    match dups {
        Ok(dups) if !dups.is_empty() => KeyError::Duplicate(dups).into(),
        _ => refusal,
    }
}

/// `dups` and then the waiting duplicates the shards found, the first
/// few.
fn collect_dups<X: Extra>(shards: &mut [Pass<'_, X>], mut dups: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
    for s in shards {
        for d in s.dups.drain(..) {
            if dups.len() < FEW && !dups.contains(&d) {
                dups.push(d);
            }
        }
    }
    dups
}

/// The suspects of the last pass, counted by the next read.
struct Pending {
    list: Vec<Suspect>,
    /// Refine half: the list and the keys.
    res: Reservation,
}

impl Pending {
    fn new(budget: &Budget) -> Result<Self> {
        Ok(Self {
            list: Vec::new(),
            res: budget.try_reserve(Half::Refine, Bytes(0))?,
        })
    }

    /// Drops the suspects the last read counted and takes the shards'
    /// suspects for the next read to count.
    fn replace<X: Extra>(&mut self, shards: &mut [Pass<'_, X>]) -> Result<()> {
        for s in self.list.drain(..) {
            self.res.shrink(Bytes(s.key.len() as u64));
        }
        for s in shards {
            self.take_from(s)?;
        }
        Ok(())
    }

    /// Moves a shard's suspects here, as many as are counted at once.
    fn take_from<X: Extra>(&mut self, shard: &mut Pass<'_, X>) -> Result<()> {
        for s in shard.suspects.drain(..) {
            let bytes = Bytes(s.key.len() as u64);
            shard.res.shrink(bytes);
            if self.list.len() < SUSPECTS {
                let budget = shard.budget;
                let mut alone = u64::MAX;
                if !grow(&mut self.list, &mut self.res, budget, &mut alone, 1) {
                    return Err(Error::Budget(BudgetExceeded {
                        half: Half::Refine,
                        requested: Bytes(size_of::<Suspect>() as u64),
                        available: budget.available(Half::Refine),
                    }));
                }
                self.res.grow(bytes)?;
                self.list.push(s);
            }
        }
        Ok(())
    }

    /// The suspects seen more than once in their file by the last read.
    fn duplicates<A: Source + ?Sized, B: Source + ?Sized>(
        &self,
        ra: &Reader<'_, A>,
        rb: &Reader<'_, B>,
    ) -> Vec<Vec<u8>> {
        let mut dups: Vec<Vec<u8>> = Vec::new();
        for (i, s) in self.list.iter().enumerate() {
            let seen = ra.seen()[i] + rb.seen()[i];
            if seen > 1 && dups.len() < FEW && !dups.contains(&s.key) {
                dups.push(s.key.clone());
            }
        }
        dups
    }
}

/// Whose record a waiting entry holds, or what became of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum State {
    /// Waiting, from A.
    A,
    /// Waiting, from B.
    B,
    /// Matched with different bytes; only the key is kept.
    Changed,
    /// Matched with equal bytes.
    Dead,
}

/// A record kept by the pass, its bytes in the arena.
#[derive(Debug, Clone, Copy)]
struct Kept<X> {
    hash: u64,
    line_a: u64,
    line_b: u64,
    /// Offset of the bytes in the arena, which stays below 4 GiB.
    start: u32,
    len: u32,
    key_off: u32,
    key_len: u32,
    table: u32,
    state: State,
    extra: X,
}

const _: () = assert!(size_of::<Kept<()>>() == 48);
const _: () = assert!(size_of::<Kept<Ranges>>() == 72);

/// What a kept record carries besides its key and lines, and the change
/// it is handed out as.
trait Extra: Copy + Send + fmt::Debug {
    type Change: Send + fmt::Debug;
    /// `offset` finds the record's file offset, for an `Extra` that
    /// keeps it.
    fn new(rec: &Record<'_>, offset: impl FnOnce() -> u64) -> Self;
    /// The other file's record for the same key, with different bytes.
    fn matched(&mut self, side: Side, rec: &Record<'_>, offset: impl FnOnce() -> u64);
    fn change(base: KeyedChange) -> Self::Change;
    fn base(change: &mut Self::Change) -> &mut KeyedChange;
    /// Fills in what `base` does not hold, for a record in A if `in_a`
    /// and in B if `in_b`.
    fn fill(&self, change: &mut Self::Change, in_a: bool, in_b: bool);
}

impl Extra for () {
    type Change = KeyedChange;
    fn new(_: &Record<'_>, _: impl FnOnce() -> u64) -> Self {}
    fn matched(&mut self, _: Side, _: &Record<'_>, _: impl FnOnce() -> u64) {}
    fn change(base: KeyedChange) -> KeyedChange {
        base
    }
    fn base(change: &mut KeyedChange) -> &mut KeyedChange {
        change
    }
    fn fill(&self, _: &mut KeyedChange, _: bool, _: bool) {}
}

/// Where the record's bytes are in A and B, for reading it back: 24
/// bytes more per kept record, paid only by [`compare_with_ranges`].
#[derive(Debug, Clone, Copy)]
struct Ranges {
    off: [u64; 2],
    len: [u32; 2],
}

impl Extra for Ranges {
    type Change = RangedChange;

    fn new(rec: &Record<'_>, offset: impl FnOnce() -> u64) -> Self {
        Self {
            off: [offset(); 2],
            len: [record_len(rec); 2],
        }
    }

    fn matched(&mut self, side: Side, rec: &Record<'_>, offset: impl FnOnce() -> u64) {
        let i = side as usize;
        (self.off[i], self.len[i]) = (offset(), record_len(rec));
    }

    fn change(change: KeyedChange) -> RangedChange {
        RangedChange {
            change,
            a_range: None,
            b_range: None,
        }
    }

    fn base(change: &mut RangedChange) -> &mut KeyedChange {
        &mut change.change
    }

    fn fill(&self, change: &mut RangedChange, in_a: bool, in_b: bool) {
        let range = |i: usize| self.off[i]..self.off[i] + u64::from(self.len[i]);
        change.a_range = in_a.then(|| range(0));
        change.b_range = in_b.then(|| range(1));
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a record lies in a window, which stays below 4 GiB"
)]
fn record_len(rec: &Record<'_>) -> u32 {
    rec.bytes.len() as u32
}

/// What a shard does after one record.
enum Flow {
    Next,
    Overflow(Half),
}

/// One shard of a pass, reused by the next pass.
#[derive(Debug)]
struct Pass<'b, X: Extra> {
    budget: &'b Budget,
    /// Open addressing, linear probing: `EMPTY`, a matched key's
    /// fingerprint, or `WAITING | index into kept`.
    slots: Vec<u32>,
    used: usize,
    limit: usize,
    /// Skeleton half: the slots.
    _table: Reservation,
    kept: Vec<Kept<X>>,
    arena: Vec<u8>,
    /// Arena bytes and entries of `kept` no longer needed.
    garbage: usize,
    dead: usize,
    dups: Vec<Vec<u8>>,
    suspects: Vec<Suspect>,
    change: X::Change,
    /// Refine half: `kept`, the arena, the duplicates, the suspects and
    /// the key of `change`.
    res: Reservation,
    /// Refine-half bytes `res` may still grow by this round, see [`grow`].
    credit: u64,
    /// `credit` when the round began.
    share: u64,
    overflow: Option<Half>,
    /// The overflow came from a record that more than half fills `share`.
    alone: bool,
    /// Bytes a duplicate needed that its list had no room for, 0 for
    /// none: the read must end in an error. Four bytes, so the shard
    /// grows by none.
    lost: u32,
    fault: Option<Error>,
}

impl<'b, X: Extra> Pass<'b, X> {
    /// A table for about `keys` keys, or as many as `share` bytes of the
    /// skeleton half hold. Too small an estimate costs passes, not
    /// correctness.
    fn new(budget: &'b Budget, keys: u64, share: u64) -> Result<Self> {
        let cap = (share / 4).min(keys.saturating_mul(5) / 4 + 64);
        let table = budget.try_reserve(Half::Skeleton, Bytes(cap * 4))?;
        let cap = usize::try_from(cap).unwrap_or(usize::MAX);
        Ok(Self {
            budget,
            slots: vec![EMPTY; cap],
            used: 0,
            // Linear probing over fingerprints stays short up to 80% load.
            limit: cap / 5 * 4,
            _table: table,
            kept: Vec::new(),
            arena: Vec::new(),
            garbage: 0,
            dead: 0,
            dups: Vec::new(),
            suspects: Vec::new(),
            change: X::change(KeyedChange {
                key: Vec::new(),
                kind: ChangeKind::Changed,
                a: None,
                b: None,
            }),
            res: budget.try_reserve(Half::Refine, Bytes(0))?,
            credit: u64::MAX,
            share: u64::MAX,
            overflow: None,
            alone: false,
            lost: 0,
            fault: None,
        })
    }

    /// `n` shards sharing the skeleton half for about `keys` keys, and
    /// the refine-half bytes of their list.
    fn shards(budget: &'b Budget, keys: u64, n: usize) -> Result<(Vec<Self>, Reservation)> {
        let share = budget.available(Half::Skeleton).0 / n as u64;
        let list = budget.try_reserve(Half::Refine, Bytes((n * size_of::<Self>()) as u64))?;
        let mut shards = Vec::with_capacity(n);
        for _ in 0..n {
            shards.push(Self::new(budget, keys / n as u64 + 1, share)?);
        }
        Ok((shards, list))
    }

    fn start(&mut self, _lo: u64, _hi: u64) {
        self.slots.fill(EMPTY);
        self.used = 0;
        self.kept.clear();
        self.arena.clear();
        self.garbage = 0;
        self.dead = 0;
        for d in self.dups.drain(..) {
            self.res.shrink(Bytes(d.len() as u64));
        }
        for s in self.suspects.drain(..) {
            self.res.shrink(Bytes(s.key.len() as u64));
        }
        // Emptied, so the next record's key fits the capacity a key as
        // long took before.
        X::base(&mut self.change).key.clear();
        self.overflow = None;
        self.alone = false;
        self.lost = 0;
        self.fault = None;
    }

    /// Frees the lists, which keep their largest size otherwise, for a
    /// read that needs their bytes elsewhere: a pass that starts over
    /// because a window could not keep a record or name a table, or the
    /// read that counts suspects. A pass that starts over because a shard
    /// ran out keeps them, since a narrower range needs no more.
    ///
    /// Suspects stay until something takes them, since the read that
    /// counts them needs them; with none left, their list goes too.
    fn release(&mut self) {
        if self.suspects.is_empty() {
            self.suspects = Vec::new();
        }
        let keep = self.suspects.capacity() * size_of::<Suspect>()
            + self.suspects.iter().map(|s| s.key.len()).sum::<usize>();
        self.kept = Vec::new();
        self.arena = Vec::new();
        self.dups = Vec::new();
        X::base(&mut self.change).key = Vec::new();
        (self.garbage, self.dead) = (0, 0);
        self.res
            .shrink(Bytes(self.res.bytes().0.saturating_sub(keep as u64)));
    }

    /// Takes this shard's records of the readers' last windows.
    fn round<A, B>(&mut self, k: usize, n: usize, ra: &Reader<'_, A>, rb: &Reader<'_, B>)
    where
        A: Source + ?Sized,
        B: Source + ?Sized,
    {
        let _ = self.feed(Side::A, k, n, ra.kept(), |r| ra.offset(r))
            && self.feed(Side::B, k, n, rb.kept(), |r| rb.offset(r));
    }

    fn feed<'r>(
        &mut self,
        side: Side,
        k: usize,
        n: usize,
        recs: impl Iterator<Item = Record<'r>>,
        offset: impl Fn(&Record<'_>) -> u64,
    ) -> bool {
        for rec in recs.filter(|r| shard(r.hash, n) == k) {
            match self.take(side, &rec, &offset) {
                Ok(Flow::Next) => {}
                Ok(Flow::Overflow(half)) => {
                    self.overflow = Some(half);
                    return false;
                }
                Err(e) => {
                    self.fault = Some(e);
                    return false;
                }
            }
        }
        true
    }

    fn home(&self, hash: u64) -> usize {
        // The range fixes the top bits of the hashes in a pass; the
        // multiply spreads the rest over the whole table.
        let mixed = hash.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        #[allow(clippy::cast_possible_truncation, reason = "below the slot count")]
        let slot = ((u128::from(mixed) * self.slots.len() as u128) >> 64) as usize;
        slot
    }

    fn next_slot(&self, i: usize) -> usize {
        if i + 1 == self.slots.len() { 0 } else { i + 1 }
    }

    fn key(&self, k: &Kept<X>) -> &[u8] {
        &self.arena[k.start as usize + k.key_off as usize..][..k.key_len as usize]
    }

    /// The slot that points at waiting record `at`.
    fn slot_of(&self, at: usize) -> usize {
        let want = WAITING | slot_index(at);
        let mut i = self.home(self.kept[at].hash);
        while self.slots[i] != want {
            i = self.next_slot(i);
        }
        i
    }

    /// Takes one record of `side`.
    fn take(
        &mut self,
        side: Side,
        rec: &Record<'_>,
        offset: &impl Fn(&Record<'_>) -> u64,
    ) -> Result<Flow> {
        let waiting = match side {
            Side::A => State::A,
            Side::B => State::B,
        };
        let key = &rec.bytes[rec.key.clone()];
        let print = fingerprint(rec.hash);
        let mut i = self.home(rec.hash);
        let mut seen = false;
        loop {
            let v = self.slots[i];
            if v == EMPTY {
                break;
            }
            if v & WAITING == 0 {
                seen |= v == print;
            } else {
                let k = self.kept[index(v)];
                if k.hash == rec.hash && k.table == rec.table && self.key(&k) == key {
                    if k.state == waiting {
                        if !self.duplicate(key) {
                            // The pass must not end as if the second record
                            // were not there, see `run`.
                            self.lost =
                                u32::try_from(size_of::<Vec<u8>>() + key.len()).unwrap_or(u32::MAX);
                        }
                    } else {
                        self.matched(index(v), i, side, rec, || offset(rec));
                    }
                    return Ok(Flow::Next);
                }
            }
            i = self.next_slot(i);
        }
        #[cfg(test)]
        {
            seen |= tests::COLLIDES.with(std::cell::Cell::get)(key);
        }
        if let Some(half) = self.room(rec.bytes.len(), key.len(), seen) {
            return Ok(Flow::Overflow(half));
        }
        let (Ok(len), Ok(key_off), Ok(key_len)) = (
            u32::try_from(rec.bytes.len()),
            u32::try_from(rec.key.start),
            u32::try_from(key.len()),
        ) else {
            return Err(Error::Keyed(KeyError::TooLong {
                side,
                line: LineNo(rec.line),
                cap: Bytes(u64::from(u32::MAX)),
            }));
        };
        let at = self.kept.len();
        self.kept.push(Kept {
            hash: rec.hash,
            line_a: rec.line,
            line_b: rec.line,
            start: arena_offset(self.arena.len()),
            len,
            key_off,
            key_len,
            table: rec.table,
            state: waiting,
            extra: X::new(rec, || offset(rec)),
        });
        self.arena.extend_from_slice(rec.bytes);
        self.slots[i] = WAITING | slot_index(at);
        self.used += 1;
        if seen {
            self.suspects.push(Suspect {
                side,
                table: rec.table,
                hash: rec.hash,
                key: key.to_vec(),
            });
        }
        Ok(Flow::Next)
    }

    /// A key found waiting from the same file; `false` when it could not
    /// be listed and none is yet.
    fn duplicate(&mut self, key: &[u8]) -> bool {
        if self.dups.len() >= FEW || self.dups.iter().any(|d| d == key) {
            return true;
        }
        #[cfg(test)]
        let full = tests::DUPS_FULL.with(std::cell::Cell::get);
        #[cfg(not(test))]
        let full = false;
        if !full
            && grow(
                &mut self.dups,
                &mut self.res,
                self.budget,
                &mut self.credit,
                1,
            )
            && spend(&mut self.res, &mut self.credit, key.len() as u64)
        {
            self.dups.push(key.to_vec());
            return true;
        }
        !self.dups.is_empty()
    }

    /// The other file's record for a waiting key: compare the bytes.
    fn matched(
        &mut self,
        at: usize,
        slot: usize,
        side: Side,
        rec: &Record<'_>,
        offset: impl FnOnce() -> u64,
    ) {
        let k = &mut self.kept[at];
        let stored = &self.arena[k.start as usize..][..k.len as usize];
        if stored == rec.bytes {
            k.state = State::Dead;
            self.dead += 1;
            self.garbage += k.len as usize;
        } else {
            k.state = State::Changed;
            match side {
                Side::A => k.line_a = rec.line,
                Side::B => k.line_b = rec.line,
            }
            k.extra.matched(side, rec, offset);
            self.garbage += (k.len - k.key_len) as usize;
        }
        self.slots[slot] = fingerprint(k.hash);
    }

    /// `None` when one more record of `len` bytes fits, with room to hand
    /// its `key` bytes out at the end of the pass and to keep them as a
    /// suspect, growing the lists from the refine half if needed; else
    /// the half that is full.
    fn room(&mut self, len: usize, key: usize, suspect: bool) -> Option<Half> {
        if self.used >= self.limit {
            return Some(Half::Skeleton);
        }
        let full = self.kept.len() == self.kept.capacity()
            || self.arena.capacity() - self.arena.len() < len;
        if full && self.partly_dead() {
            self.compact();
        }
        let fits = self.kept.len() < (WAITING - 1) as usize
            && u32::try_from(self.arena.len() + len).is_ok()
            && grow(
                &mut self.kept,
                &mut self.res,
                self.budget,
                &mut self.credit,
                1,
            )
            && grow(
                &mut self.arena,
                &mut self.res,
                self.budget,
                &mut self.credit,
                len,
            )
            && grow(
                &mut X::base(&mut self.change).key,
                &mut self.res,
                self.budget,
                &mut self.credit,
                key,
            )
            && (!suspect
                || (grow(
                    &mut self.suspects,
                    &mut self.res,
                    self.budget,
                    &mut self.credit,
                    1,
                ) && spend(&mut self.res, &mut self.credit, key as u64)));
        if fits {
            return None;
        }
        self.alone = (len + key) as u64 > self.share / 2;
        Some(Half::Refine)
    }

    /// Whether a quarter of the kept bytes or entries are dead: the lists
    /// are full, and compacting them is cheaper than growing them.
    fn partly_dead(&self) -> bool {
        (self.garbage > 0 && 4 * self.garbage >= self.arena.len())
            || (self.dead > 0 && 4 * self.dead >= self.kept.len())
    }

    /// Moves live entries and their bytes down over the dead ones.
    fn compact(&mut self) {
        let (mut w, mut aw) = (0, 0);
        for r in 0..self.kept.len() {
            let mut k = self.kept[r];
            let (from, n) = match k.state {
                State::Dead => continue,
                State::A | State::B => (k.start as usize, k.len as usize),
                State::Changed => (k.start as usize + k.key_off as usize, k.key_len as usize),
            };
            if k.state != State::Changed && r != w {
                let slot = self.slot_of(r);
                self.slots[slot] = WAITING | slot_index(w);
            }
            self.arena.copy_within(from..from + n, aw);
            k.start = arena_offset(aw);
            aw += n;
            if k.state == State::Changed {
                k.key_off = 0;
                k.len = k.key_len;
            }
            self.kept[w] = k;
            w += 1;
        }
        self.kept.truncate(w);
        self.arena.truncate(aw);
        self.garbage = 0;
        self.dead = 0;
    }

    /// Hands the change at `kept[i]` to `on_change`.
    fn emit_one(
        &mut self,
        i: usize,
        on_change: &mut impl FnMut(&X::Change) -> io::Result<()>,
    ) -> io::Result<()> {
        let k = self.kept[i];
        let change = X::base(&mut self.change);
        change.key.clear();
        change.key.extend_from_slice(
            &self.arena[k.start as usize + k.key_off as usize..][..k.key_len as usize],
        );
        change.kind = match k.state {
            State::A => ChangeKind::Removed,
            State::B => ChangeKind::Added,
            State::Changed | State::Dead => ChangeKind::Changed,
        };
        let in_a = change.kind != ChangeKind::Added;
        let in_b = change.kind != ChangeKind::Removed;
        change.a = in_a.then_some(LineNo(k.line_a));
        change.b = in_b.then_some(LineNo(k.line_b));
        k.extra.fill(&mut self.change, in_a, in_b);
        on_change(&self.change)
    }
}

/// Where a change sorts: by its line, in B for an added record.
fn order<X>(k: &Kept<X>) -> (u64, State) {
    (
        if k.state == State::B {
            k.line_b
        } else {
            k.line_a
        },
        k.state,
    )
}

/// Hands the pass's changes to `on_change`, ordered by line across the
/// shards.
fn emit<X: Extra>(
    shards: &mut [Pass<'_, X>],
    on_change: &mut impl FnMut(&X::Change) -> io::Result<()>,
) -> Result<u64> {
    for s in shards.iter_mut() {
        s.kept.retain(|k| k.state != State::Dead);
        s.kept.sort_unstable_by_key(order);
    }
    let mut at = [0usize; MAX_SHARDS];
    let mut count = 0;
    loop {
        let next = shards
            .iter()
            .enumerate()
            .filter(|(k, s)| at[*k] < s.kept.len())
            .min_by_key(|(k, s)| order(&s.kept[at[*k]]))
            .map(|(k, _)| k);
        let Some(k) = next else {
            return Ok(count);
        };
        shards[k].emit_one(at[k], on_change)?;
        at[k] += 1;
        count += 1;
    }
}

/// The shard of `n` that owns a key hash, from bits the table's slot and
/// fingerprint barely use.
fn shard(hash: u64, n: usize) -> usize {
    #[allow(clippy::cast_possible_truncation, reason = "below n")]
    let s = ((((hash >> 33) & 0xFFFF) * n as u64) >> 16) as usize;
    s
}

/// 31 bits of a key hash, never 0, so they fit a slot with its flag.
#[allow(clippy::cast_possible_truncation, reason = "31 bits kept")]
fn fingerprint(hash: u64) -> u32 {
    ((hash >> 1) as u32 & !WAITING).max(1)
}

fn index(v: u32) -> usize {
    (v & !WAITING) as usize
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "room keeps kept below 2^31 entries"
)]
fn slot_index(i: usize) -> u32 {
    i as u32
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "room keeps the arena below 4 GiB"
)]
fn arena_offset(i: usize) -> u32 {
    i as u32
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fmt::Write;

    use proptest::prelude::*;

    use super::*;
    use crate::keyed::KeySpec;
    use crate::keyed::read::key_hash;

    thread_local! {
        /// Keys this thread's shards take for suspects, as if their
        /// fingerprint matched a matched key's: collisions on demand.
        pub(super) static COLLIDES: std::cell::Cell<fn(&[u8]) -> bool> =
            const { std::cell::Cell::new(|_| false) };
        /// Whether this thread's shards find no room to list a duplicate.
        pub(super) static DUPS_FULL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    type Change = (ChangeKind, String, Option<u64>, Option<u64>);
    type Rows = Vec<(u32, u32)>;

    /// [`compare`] with no limit on the passes, for tests of what many
    /// passes find.
    fn run(a: &[u8], b: &[u8], spec: &str, mem: u64) -> Result<(Vec<Change>, Report)> {
        limited(a, b, spec, mem, u64::MAX)
    }

    /// [`compare`] with at most `limit` passes.
    fn limited(
        a: &[u8],
        b: &[u8],
        spec: &str,
        mem: u64,
        limit: u64,
    ) -> Result<(Vec<Change>, Report)> {
        let spec: KeySpec = spec.parse().unwrap();
        let budget = Budget::new(Bytes(mem));
        let mut out = Vec::new();
        let report = super::run::<_, _, ()>(a, b, &spec, &budget, limit, |c| {
            out.push((
                c.kind,
                String::from_utf8_lossy(&c.key).into_owned(),
                c.a.map(|l| l.0),
                c.b.map(|l| l.0),
            ));
            Ok(())
        })
        .map_err(|e| doubled(e, &budget))?;
        out.sort_unstable_by(|x, y| x.1.cmp(&y.1));
        Ok((out, report))
    }

    /// The changes between two `key,value` files, from hash maps.
    fn expected(a: &[(u32, u32)], b: &[(u32, u32)]) -> Vec<Change> {
        let index = |rows: &[(u32, u32)]| -> HashMap<u32, (u32, u64)> {
            rows.iter()
                .enumerate()
                .map(|(i, &(k, v))| (k, (v, i as u64)))
                .collect()
        };
        let (ia, ib) = (index(a), index(b));
        let mut out = Vec::new();
        for (k, &(v, la)) in &ia {
            match ib.get(k) {
                None => out.push((ChangeKind::Removed, k.to_string(), Some(la), None)),
                Some(&(w, lb)) if w != v => {
                    out.push((ChangeKind::Changed, k.to_string(), Some(la), Some(lb)));
                }
                Some(_) => {}
            }
        }
        for (k, &(_, lb)) in &ib {
            if !ia.contains_key(k) {
                out.push((ChangeKind::Added, k.to_string(), None, Some(lb)));
            }
        }
        out.sort_unstable_by(|x, y| x.1.cmp(&y.1));
        out
    }

    fn csv(rows: &[(u32, u32)]) -> Vec<u8> {
        let mut s = String::new();
        for (k, v) in rows {
            writeln!(s, "{k},{v}").unwrap();
        }
        s.into_bytes()
    }

    /// `n` distinct keys, then B as A shuffled in blocks with changes.
    fn pair(n: u32, seed: u64) -> (Rows, Rows) {
        let mut state = seed | 1;
        let mut rnd = |m: u32| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            u32::try_from(state % u64::from(m)).unwrap()
        };
        let a: Rows = (0..n).map(|key| (key * 7 + 3, rnd(1000))).collect();
        let mut b = a.clone();
        for _ in 0..n / 20 {
            let (from, to) = (rnd(n) as usize, rnd(n) as usize);
            b.swap(from, to);
        }
        for added in 0..n / 50 {
            let at = rnd(n) as usize % b.len();
            match rnd(3) {
                0 => b[at].1 += 1,
                1 => {
                    b.remove(at);
                }
                _ => b.insert(at, ((n + added) * 7 + 1, 0)),
            }
        }
        let third = b.len() / 3;
        b.rotate_left(third);
        (a, b)
    }

    #[test]
    fn reordering_alone_is_no_change() {
        let (a, _) = pair(500, 3);
        let mut b = a.clone();
        b.reverse();
        let (changes, report) = run(&csv(&a), &csv(&b), "csv:1", 1 << 20).unwrap();
        assert_eq!(changes, []);
        assert_eq!(
            (report.passes, report.records_a, report.records_b),
            (1, 500, 500)
        );
    }

    #[test]
    fn small_budgets_take_more_passes_with_the_same_result() {
        // Miri: 300 rows and 8 and 12 KiB budgets (13 and 7 passes); the
        // native run keeps 3000 rows and 24 and 64 KiB.
        let (n, one_pass, budgets, floor) = if cfg!(miri) {
            (300, 1 << 20, [8 << 10, 12 << 10], 5)
        } else {
            (3000, 1 << 22, [24 << 10, 64 << 10], 50)
        };
        let (a, b) = pair(n, 7);
        let want = expected(&a, &b);
        assert!(want.len() > floor);
        let (ca, cb) = (csv(&a), csv(&b));
        let (one, report) = run(&ca, &cb, "csv:1", one_pass).unwrap();
        assert_eq!(report.passes, 1);
        assert_eq!(one, want);
        for mem in budgets {
            let (many, report) = run(&ca, &cb, "csv:1", mem).unwrap();
            assert!(report.passes > 1, "{mem}: {report:?}");
            assert_eq!(report.changes, want.len() as u64);
            assert_eq!(many, want, "{mem}");
        }
    }

    #[test]
    fn duplicates_are_errors_wherever_they_are() {
        // Waiting in the same file, and after the key was matched.
        for (a, b) in [
            (&b"1,a\n1,b\n"[..], &b"2,c\n"[..]),
            (b"1,a\n", b"1,a\n2,b\n1,c\n"),
            (b"1,a\n2,x\n1,a\n", b"1,a\n"),
        ] {
            let err = run(a, b, "csv:1", 1 << 20).unwrap_err();
            assert!(
                matches!(&err, Error::Keyed(KeyError::Duplicate(k)) if k == &[b"1".to_vec()]),
                "{err}"
            );
        }
        let rows: Vec<u8> = (0..20)
            .flat_map(|i| format!("{},x\n", i % 7).into_bytes())
            .collect();
        let Err(Error::Keyed(KeyError::Duplicate(keys))) = run(&rows, b"", "csv:1", 1 << 20) else {
            panic!("no error");
        };
        assert_eq!(keys.len(), FEW);
    }

    #[test]
    fn changed_records_report_both_lines() {
        let a = b"id,v\n1,x\n2,y\n3,z\n";
        let b = b"id,v\n3,z\n4,w\n1,X\n";
        let (changes, _) = run(a, b, "csv:id", 1 << 20).unwrap();
        assert_eq!(
            changes,
            [
                (ChangeKind::Changed, "1".into(), Some(1), Some(3)),
                (ChangeKind::Removed, "2".into(), Some(2), None),
                (ChangeKind::Added, "4".into(), None, Some(2)),
            ]
        );
    }

    type Records = Vec<(String, Option<Vec<u8>>, Option<Vec<u8>>)>;

    /// Each change's key and the bytes its ranges cover in A and B.
    fn records(a: &[u8], b: &[u8], mem: u64) -> Records {
        let cut = |file: &[u8], r: &Option<std::ops::Range<u64>>| {
            r.as_ref().map(|r| {
                file[usize::try_from(r.start).unwrap()..usize::try_from(r.end).unwrap()].to_vec()
            })
        };
        let spec: KeySpec = "csv:1".parse().unwrap();
        let mut out = Vec::new();
        super::run::<_, _, Ranges>(a, b, &spec, &Budget::new(Bytes(mem)), u64::MAX, |c| {
            let key = String::from_utf8_lossy(&c.change.key).into_owned();
            out.push((key, cut(a, &c.a_range), cut(b, &c.b_range)));
            Ok(())
        })
        .unwrap();
        out.sort_unstable();
        out
    }

    #[test]
    fn changes_carry_the_byte_ranges_of_their_records() {
        let a = b"id,v\r\n1,x\r\n2,\"y\r\ny\"\r\n3,z\r\n";
        let b = b"id,v\n3,z\n4,\"w\"\"\nw\"\n1,X";
        let some = |s: &[u8]| Some(s.to_vec());
        assert_eq!(
            records(a, b, 1 << 20),
            [
                ("1".into(), some(b"1,x"), some(b"1,X")),
                ("2".into(), some(b"2,\"y\r\ny\""), None),
                ("4".into(), None, some(b"4,\"w\"\"\nw\"")),
            ]
        );
    }

    proptest! {
        #[test]
        #[cfg_attr(miri, ignore = "too slow under Miri")]
        fn ranges_hold_their_records_under_any_budget(n in 1u32..1500, seed in any::<u64>(), mem in 16u64..256) {
            let (a, b) = pair(n, seed);
            let (fa, fb) = (csv(&a), csv(&b));
            let line = |rows: &Rows, k: &str| {
                rows.iter().find(|r| r.0.to_string() == k).map(|(k, v)| format!("{k},{v}").into_bytes())
            };
            for (key, ra, rb) in records(&fa, &fb, mem << 10) {
                prop_assert_eq!(&ra, &line(&a, &key));
                prop_assert_eq!(&rb, &line(&b, &key));
            }
        }
    }

    #[test]
    fn copy_keys_of_different_tables_do_not_meet() {
        let a = b"COPY t (id, v) FROM stdin;\n1\tx\n\\.\nCOPY u (id, v) FROM stdin;\n1\ty\n\\.\n";
        let b = b"COPY u (id, v) FROM stdin;\n1\ty\n\\.\nCOPY t (id, v) FROM stdin;\n1\tz\n\\.\n";
        let (changes, _) = run(a, b, "copy:id", 1 << 20).unwrap();
        assert_eq!(
            changes,
            [(ChangeKind::Changed, "1".into(), Some(1), Some(4))]
        );
        assert!(matches!(
            run(a, b, "copy:nope", 1 << 20),
            Err(Error::Keyed(KeyError::NoColumn { .. }))
        ));
    }

    #[test]
    fn a_tiny_budget_is_a_budget_error() {
        let err = run(b"1,a\n", b"1,b\n", "csv:1", 64).unwrap_err();
        assert!(matches!(err, Error::Budget(_)), "{err}");
    }

    /// A `pg_dump` keyed by `id`: `tables` tables of `rows` rows, in
    /// reverse table order with every 7th row changed if `b`.
    fn dump(tables: usize, rows: usize, b: bool) -> Vec<u8> {
        let mut blocks: Vec<String> = (0..tables)
            .map(|t| {
                let mut s = format!("COPY public.t{t} (id, v) FROM stdin;\n");
                for id in 0..rows {
                    let v = if b && id % 7 == 0 { "changed" } else { "same" };
                    writeln!(
                        s,
                        "{id}\t{v} padding padding padding padding padding padding padding padding"
                    )
                    .unwrap();
                }
                s + "\\.\n\nSELECT 1;\n\n"
            })
            .collect();
        if b {
            blocks.reverse();
        }
        blocks.concat().into_bytes()
    }

    /// Budgets as the command line gives them, 48 KiB below `--mem`.
    fn cli(mib: u64) -> u64 {
        (mib << 20) - (48 << 10)
    }

    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn table_names_never_refuse_a_budget_that_holds_the_pass() {
        let (a, b) = (dump(1200, 15, false), dump(1200, 15, true));
        for mib in [4, 5, 6, 8] {
            let (changes, _) =
                run(&a, &b, "copy:id", cli(mib)).unwrap_or_else(|e| panic!("{mib}M: {e}"));
            assert_eq!(changes.len(), 1200 * 3, "{mib}M");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn a_record_longer_than_a_shard_share_still_fits_the_half() {
        let long = "y".repeat(400_000);
        let (mut a, mut b) = (String::from("id,v\n"), String::from("id,v\n"));
        for j in 0..3 {
            writeln!(a, "long{j},{long}").unwrap();
        }
        for i in 0..4000 {
            writeln!(a, "{i},short").unwrap();
            writeln!(b, "{i},short").unwrap();
        }
        for j in 0..3 {
            writeln!(b, "long{j},{long}z").unwrap();
        }
        for mib in [4, 5, 6] {
            let (changes, _) = run(a.as_bytes(), b.as_bytes(), "csv:id", cli(mib))
                .unwrap_or_else(|e| panic!("{mib}M: {e}"));
            assert_eq!(changes.len(), 3, "{mib}M");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn long_changed_keys_take_more_passes_not_an_error() {
        let rows = |v: &str| -> String {
            let mut s = String::from("id,v\n");
            for i in 0..40 {
                writeln!(s, "{i:08}{},{v}", "k".repeat(50_000 - 8)).unwrap();
            }
            s
        };
        let (a, b) = (rows("x"), rows("y"));
        for mib in [1, 2, 4] {
            let (changes, _) = run(a.as_bytes(), b.as_bytes(), "csv:1", cli(mib))
                .unwrap_or_else(|e| panic!("{mib}M: {e}"));
            assert_eq!(changes.len(), 40, "{mib}M");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn every_budget_refusal_asks_for_twice_the_budget() {
        let long = format!("1,{}\n", "x".repeat(300_000));
        let (a, b) = (dump(1200, 15, false), dump(1200, 15, true));
        let small: &[u64] = &[64, 4 << 10, 64 << 10, 200 << 10];
        let cases = [
            (long.as_bytes(), &b"1,b\n"[..], "csv:1", small),
            (&a, &b, "copy:id", &[cli(2), cli(3)]),
        ];
        for (a, b, spec, budgets) in cases {
            for &mem in budgets {
                let Err(Error::Budget(refusal)) = run(a, b, spec, mem) else {
                    continue;
                };
                let advice = refusal.mem_advice(Bytes(mem));
                assert!(
                    advice.is_some_and(|m| m.0 >= 2 * mem),
                    "{spec} at {mem}: {refusal} advises {advice:?}"
                );
            }
        }
    }

    /// The passes `a` and `b` take at `mem`, after checking that every
    /// limit below them refuses or answers the same, and none at or above
    /// them refuses; and the largest limit that refused.
    fn limits_hold(a: &[u8], b: &[u8], mem: u64) -> (u64, u64) {
        let (want, report) = run(a, b, "csv:1", mem).unwrap();
        let passes = u64::from(report.passes);
        let mut refused = 0;
        for limit in 1..=passes + 1 {
            match limited(a, b, "csv:1", mem, limit) {
                Ok((got, _)) => assert_eq!(got, want, "{mem}, limit {limit}"),
                Err(Error::Budget(_)) => {
                    assert!(passes > limit, "{mem}: {passes} passes refused at {limit}");
                    refused = limit;
                }
                Err(e) => panic!("{mem}, limit {limit}: {e}"),
            }
        }
        (passes, refused)
    }

    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn the_pass_limit_refuses_only_runs_that_exceed_it() {
        let (a, b) = pair(3000, 7);
        let (ca, cb) = (csv(&a), csv(&b));
        for mem in [16 << 10, 24 << 10, 48 << 10] {
            let (passes, refused) = limits_hold(&ca, &cb, mem);
            assert!(passes > 4, "{mem}: {passes} passes");
            // A lower bound, and a close one: 33, 21 and 10 passes are
            // refused from limits of 32, 20 and 9.
            assert!(
                refused + 1 >= passes,
                "{mem}: {passes} passes, refused up to {refused}"
            );
        }
    }

    /// Keys whose hashes crowd one narrow band, the rest spread out, in
    /// an order of their own: a range that holds the band says nothing of
    /// the ranges after it, and the bound must not take it for them.
    fn crowded() -> (Vec<u8>, Vec<u8>) {
        let from = END / 8;
        let mut keys: Vec<String> = (0u64..)
            .map(|j| format!("k{j}"))
            .filter(|k| (from..from + (END >> 10)).contains(&key_hash(k.as_bytes(), 0)))
            .take(4000)
            .collect();
        keys.extend((0..40_000).map(|j| format!("u{j}")));
        keys.sort_by_key(|k| key_hash(k.as_bytes(), 99));
        let (mut a, mut b) = (String::from("id,v\n"), String::from("id,v\n"));
        for (i, k) in keys.iter().enumerate() {
            writeln!(a, "{k},v").unwrap();
            writeln!(b, "{k},{}", if i % 1000 == 0 { "w" } else { "v" }).unwrap();
        }
        (a.into_bytes(), b.into_bytes())
    }

    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn the_pass_limit_holds_on_keys_crowded_into_one_hash_band() {
        let (a, b) = crowded();
        for mem in [24 << 10, 32 << 10, 48 << 10, 64 << 10] {
            let (want, report) = run(&a, &b, "csv:1", mem).unwrap();
            let passes = u64::from(report.passes);
            let (got, _) = limited(&a, &b, "csv:1", mem, passes)
                .unwrap_or_else(|e| panic!("{mem}: {passes} passes refused: {e}"));
            assert_eq!(got, want, "{mem}");
        }
    }

    /// Files in the same order: after the first pass, the check at the
    /// end of each pass is what refuses a run one pass over the limit.
    /// Runs of 22 and 10 passes are refused from limits of 21 and 9, and
    /// only from 20 and 8 without that check.
    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn the_pass_limit_is_close_on_files_in_the_same_order() {
        let (mut a, mut b) = (String::new(), String::new());
        for i in 0..20_000 {
            writeln!(a, "k{i},v").unwrap();
            writeln!(b, "k{i},{}", if i % 7 == 0 { "w" } else { "v" }).unwrap();
        }
        for mem in [32 << 10, 64 << 10] {
            let (passes, refused) = limits_hold(a.as_bytes(), b.as_bytes(), mem);
            assert!(passes > 4, "{mem}: {passes} passes");
            assert!(
                refused + 1 >= passes,
                "{mem}: {passes} passes, refused up to {refused}"
            );
        }
    }

    /// Many short records and then a few long ones, or the other way
    /// round: the keys met early say little of how many are left, and a
    /// run is refused only if it takes more passes than the limit.
    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn the_pass_limit_holds_when_records_change_length() {
        let file = |dense_first: bool, b: bool| {
            let v = |i: usize| if b && i.is_multiple_of(7) { "c" } else { "s" };
            let (mut short, mut long) = (String::new(), String::new());
            for i in 0..15_000 {
                writeln!(short, "s{i},{}", v(i)).unwrap();
            }
            for j in 0..500 {
                writeln!(long, "l{j},{}{}", v(j), "x".repeat(12_000)).unwrap();
            }
            let body = if dense_first {
                short + &long
            } else {
                long + &short
            };
            format!("id,v\n{body}").into_bytes()
        };
        let mem = 384 << 10;
        for dense_first in [true, false] {
            let (a, b) = (file(dense_first, false), file(dense_first, true));
            let (want, report) = run(&a, &b, "csv:1", mem).unwrap();
            let passes = u64::from(report.passes);
            assert!(passes > 2, "{dense_first}: {passes} passes");
            let (got, _) = limited(&a, &b, "csv:1", mem, passes)
                .unwrap_or_else(|e| panic!("{dense_first}: {passes} passes refused: {e}"));
            assert_eq!(got, want, "{dense_first}");
        }
    }

    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn following_the_advice_fits_the_run_in_a_few_reruns() {
        // Each refusal asks for at least twice the budget, so from a budget
        // that needs about 4 to 8 times as much, 3 reruns are enough.
        let (a, b) = pair(3000, 7);
        let uniform = (csv(&a), csv(&b));
        for (name, (a, b), mut mem) in [
            ("uniform", uniform, 16 << 10),
            ("crowded", crowded(), 8 << 10),
        ] {
            let mut reruns = 0;
            loop {
                match limited(&a, &b, "csv:1", mem, 8) {
                    Ok((_, report)) => {
                        assert!(report.passes <= 8, "{name}: {report:?}");
                        break;
                    }
                    Err(Error::Budget(refusal)) => {
                        let next = refusal.mem_advice(Bytes(mem)).unwrap().0;
                        assert!(next >= 2 * mem, "{name}: {mem} advised {next}");
                        mem = next;
                        reruns += 1;
                        assert!(reruns <= 3, "{name}: no answer within 3 reruns, at {mem}");
                    }
                    Err(e) => panic!("{name}: {e}"),
                }
            }
            assert!(reruns > 0, "{name}: the first budget was enough");
        }
    }

    #[test]
    fn an_error_in_the_data_wins_over_a_window_short_of_room() {
        let short = || {
            Err(Error::Budget(BudgetExceeded {
                half: Half::Refine,
                requested: Bytes(32),
                available: Bytes(0),
            }))
        };
        let no_key = || {
            Err(Error::Keyed(KeyError::NoKey {
                side: Side::B,
                line: LineNo(3),
            }))
        };
        for (a, b) in [
            (short(), no_key()),
            (no_key(), short()),
            (Ok(true), no_key()),
        ] {
            assert!(matches!(
                both(a, b),
                Err(Error::Keyed(KeyError::NoKey { .. }))
            ));
        }
        assert!(matches!(both(Ok(true), short()), Err(Error::Budget(_))));
        assert!(matches!(both(Ok(true), Ok(false)), Ok((true, false))));
    }

    /// The pass before a stop leaves suspects that are not duplicates,
    /// and the stopped pass's 63rd suspect is a real duplicate. Counting
    /// the old suspects first left no room for it, and the run reported
    /// collisions. Where the passes split moves the bands across them, so
    /// several positions are tried.
    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn a_duplicate_after_a_stop_is_named_not_taken_for_collisions() {
        // Single shard at this budget, so the shards run on this thread.
        let mem = 128 << 10;
        let band = |from: u64, width: u64, prefix: &str, n: usize| -> Vec<String> {
            (0..)
                .map(|j| format!("{prefix}{j}"))
                .filter(|k| (from..from + width).contains(&key_hash(k.as_bytes(), 0)))
                .take(n)
                .collect()
        };
        COLLIDES.with(|c| c.set(|key| key.first().is_some_and(|b| b"cs".contains(b))));
        let mut fill = String::new();
        for i in 0..20_000 {
            writeln!(fill, "k{i},v").unwrap();
        }
        for at in 1..8 {
            let from = END / 8 * at;
            let suspects = band(from, END >> 10, &format!("c{at}_"), 70);
            let dup = band(from, END >> 10, &format!("e{at}_"), 1).remove(0);
            let stale = band(from - END / 10, END / 10, &format!("s{at}_"), 12);
            let mut a = fill.clone();
            writeln!(a, "{dup},v").unwrap();
            for key in &suspects[..62] {
                writeln!(a, "{key},v").unwrap();
            }
            writeln!(a, "{dup},v").unwrap();
            for key in suspects[62..].iter().chain(&stale) {
                writeln!(a, "{key},v").unwrap();
            }
            let b = format!("{dup},v\n{fill}");
            let err = run(a.as_bytes(), b.as_bytes(), "csv:1", mem).unwrap_err();
            let named = matches!(&err, Error::Keyed(KeyError::Duplicate(keys))
                if keys.iter().any(|k| *k == dup.as_bytes()));
            assert!(named, "band {at}: {err}");
        }
        COLLIDES.with(|c| c.set(|_| false));
    }

    /// A `key,value` pair where every key in the top half of the hash
    /// range comes a second time in A, with other bytes, after all the
    /// rest.
    fn duplicated_top_half() -> (Vec<u8>, Vec<u8>) {
        let (a, b) = pair(60_000, 7);
        let mut ca = csv(&a);
        for &(k, v) in &a {
            if key_hash(k.to_string().as_bytes(), 0) >= END / 2 {
                ca.extend_from_slice(format!("{k},{}\n", v + 1).as_bytes());
            }
        }
        (ca, csv(&b))
    }

    /// A budget at which the shrunken ceiling used to refuse after a pass
    /// had met the late copies.
    const BUDGETS_WITH_DUPS: [u64; 1] = [128 << 10];

    /// Budgets as the command line gives them for `--mem` in KiB.
    fn cli_kib(kib: u64) -> u64 {
        (kib - 48) << 10
    }

    /// Reruns [`following_the_advice_reaches_the_duplicate_keys`] allows.
    const STEPS: u32 = 4;

    /// The late copies wait as suspects and crowd the refine half. Once a
    /// pass has met them, the run reports the duplicates, not a budget
    /// refusal.
    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn duplicates_win_over_a_budget_refusal() {
        let (ca, cb) = duplicated_top_half();
        for mem in BUDGETS_WITH_DUPS {
            let err = limited(&ca, &cb, "csv:1", mem, MAX_PASSES).unwrap_err();
            assert!(
                matches!(err, Error::Keyed(KeyError::Duplicate(_))),
                "{mem}: {err}"
            );
        }
    }

    /// With no room to list a duplicate, the run must end in an error,
    /// not as if the second record were not there.
    #[test]
    fn a_duplicate_with_no_room_to_list_it_is_never_dropped() {
        // Single shard at this budget, so the shards run on this thread.
        let (a, b) = (b"1,a\n2,x\n1,b\n", b"2,x\n");
        let mem = 1 << 20;
        DUPS_FULL.with(|f| f.set(true));
        let got = run(a, b, "csv:1", mem);
        DUPS_FULL.with(|f| f.set(false));
        let Err(Error::Budget(refusal)) = got else {
            panic!("{got:?}");
        };
        // At the advised budget the list has room, and names the key.
        let advised = refusal.mem_advice(Bytes(mem)).unwrap().0;
        let err = run(a, b, "csv:1", advised).unwrap_err();
        assert!(
            matches!(&err, Error::Keyed(KeyError::Duplicate(k)) if k == &[b"1".to_vec()]),
            "{err}"
        );
    }

    /// From the smallest budget the command line takes, following the
    /// advice on the same file reaches the duplicate-key error.
    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn following_the_advice_reaches_the_duplicate_keys() {
        let (ca, cb) = duplicated_top_half();
        let mut mem = cli_kib(64);
        let mut reruns = 0;
        loop {
            match limited(&ca, &cb, "csv:1", mem, MAX_PASSES) {
                Err(Error::Keyed(KeyError::Duplicate(_))) => break,
                Err(Error::Budget(refusal)) => {
                    mem = refusal.mem_advice(Bytes(mem)).unwrap().0;
                    reruns += 1;
                    assert!(
                        reruns <= STEPS,
                        "no duplicate error within {STEPS} reruns, at {mem}"
                    );
                }
                other => panic!("at {mem}: {other:?}"),
            }
        }
    }

    #[test]
    fn a_stopped_read_counts_its_own_suspects() {
        // The last pass left suspects the stopped read already counted;
        // the read that settles the stop must count the new ones instead.
        let budget = Budget::new(Bytes(1 << 20));
        let suspect = |key: &[u8]| Suspect {
            side: Side::A,
            table: 0,
            hash: 1,
            key: key.to_vec(),
        };
        let mut pending = Pending::new(&budget).unwrap();
        for i in 0..SUSPECTS {
            pending.list.push(suspect(format!("old{i}").as_bytes()));
        }
        let (mut shards, _list) = Pass::<()>::shards(&budget, 10, 1).unwrap();
        shards[0].suspects.push(suspect(b"d2"));
        pending.replace(&mut shards).unwrap();
        let keys: Vec<&[u8]> = pending.list.iter().map(|s| &s.key[..]).collect();
        assert_eq!(keys, [b"d2"]);
    }

    #[test]
    fn a_byte_order_mark_is_not_part_of_the_csv_header() {
        let (a, b) = (b"\xEF\xBB\xBFid,v\n1,a\n2,b\n", b"id,v\n2,b\n1,a\n");
        for spec in ["csv:id", "csv:1"] {
            assert_eq!(run(a, b, spec, 1 << 20).unwrap().0, [], "{spec}");
        }
        let changed = records(b"\xEF\xBB\xBFid,v\n1,a\n", b"id,v\n1,b\n", 1 << 20);
        assert_eq!(
            changed,
            [("1".into(), Some(b"1,a".to_vec()), Some(b"1,b".to_vec()))]
        );
    }

    #[test]
    fn blank_ndjson_lines_are_not_records() {
        let (a, b) = (
            b"{\"id\":1}\n  \n\t\r\n{\"id\":2}\n",
            b"{\"id\":2}\n \n{\"id\":1}\n",
        );
        let (changes, report) = run(a, b, "json:id", 1 << 20).unwrap();
        assert_eq!(changes, []);
        assert_eq!((report.records_a, report.records_b), (2, 2));
    }

    proptest! {
        #[test]
        #[cfg_attr(miri, ignore = "too slow under Miri")]
        fn any_budget_finds_the_hash_map_answer(n in 1u32..1500, seed in any::<u64>(), mem in 16u64..256) {
            let (a, b) = pair(n, seed);
            let (got, _) = run(&csv(&a), &csv(&b), "csv:1", mem << 10).unwrap();
            prop_assert_eq!(got, expected(&a, &b));
        }
    }
}
