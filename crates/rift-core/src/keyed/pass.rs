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
//! parallel. When a shard's table or waiting records outgrow the budget,
//! the pass starts over on a narrower range, sized from how far it had
//! read: a fingerprint cannot say which keys lie above a new top, so the
//! table cannot be thinned in place. Nothing is ever written to disk.
//!
//! A key whose fingerprint matches a matched key is a suspect: a
//! duplicate, or another key. The next pass, or a last read when there is
//! none, counts the suspects in their file.

use std::io;
use std::sync::Mutex;

use rayon::prelude::*;

use super::read::{Filter, Reader, Record, SUSPECTS, Suspect, Tables, grow};
use super::{ChangeKind, KeyError, KeyedChange, Side};
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
/// Refine-half bytes a shard should have to itself.
const SHARD_BYTES: u64 = 1 << 20;
/// Most shards and most chunks per window.
const MAX_SHARDS: usize = 64;
/// Refine-half bytes held for each thread of the pool.
const POOL_PER_THREAD: u64 = 16 << 10;
/// Fewest refine-half bytes for more than one thread.
const PARALLEL_REFINE: u64 = 8 << 20;

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
/// fits, an I/O error from a source or from `on_change`.
pub fn compare<A, B>(
    a: &A,
    b: &B,
    spec: &super::KeySpec,
    budget: &Budget,
    mut on_change: impl FnMut(&KeyedChange) -> io::Result<()>,
) -> Result<Report>
where
    A: Source + ?Sized,
    B: Source + ?Sized,
{
    let (threads, _pool) = threads(budget)?;
    let refine = budget.available(Half::Refine).0;
    let (len_a, len_b) = (a.len().0, b.len().0);
    // Windows in proportion to the files keep both at the same relative
    // position.
    let window = (refine / 16).clamp(64, WINDOW);
    let longest = len_a.max(len_b).max(1);
    let win = |len: u64| {
        let w = u128::from(window) * u128::from(len) / u128::from(longest);
        usize::try_from(w).unwrap_or(usize::MAX).max(64)
    };
    // A chunk per thread once the budget is large enough to care.
    let chunks = usize::try_from(refine / SHARD_BYTES)
        .unwrap_or(usize::MAX)
        .clamp(1, threads);
    let mut ra = Reader::new(a, Side::A, spec, budget, win(len_a), chunks)?;
    let mut rb = Reader::new(b, Side::B, spec, budget, win(len_b), chunks)?;
    let tables = Mutex::new(Tables::new(budget)?);
    let keys = ra.estimate_lines()? + rb.estimate_lines()?;
    let (mut shards, _list) = Pass::shards(budget, keys, chunks)?;
    let mut pending = Pending::new(budget)?;
    let mut report = Report::default();
    let (mut lo, mut width) = (0, END);
    // Below the narrowest range that ran out of waiting room.
    let mut ceiling = END;
    loop {
        let hi = lo + width.min(END - lo);
        let mut readers = (&mut ra, &mut rb, &tables);
        match read_pass(&mut readers, &mut shards, lo, hi, &pending.list)? {
            Outcome::Overflow(half, progress) => {
                let w = hi - lo;
                if w <= 1 {
                    return Err(Error::Budget(BudgetExceeded {
                        half,
                        requested: Bytes(8),
                        available: budget.available(half),
                    }));
                }
                width = narrowed(half, w, progress, fill(&shards), &mut ceiling);
                ra.shrink_lists();
                rb.shrink_lists();
            }
            Outcome::Stop => {
                let mut dups = collect_dups(&mut shards);
                if dups.is_empty() {
                    // Too many suspects: settle the first of them now.
                    for s in &mut shards {
                        pending.take_from(s)?;
                    }
                    let mut readers = (&mut ra, &mut rb, &tables);
                    read_pass(&mut readers, &mut shards, END, END, &pending.list)?;
                    dups = pending.duplicates(&ra, &rb);
                }
                return Err(if dups.is_empty() {
                    KeyError::Collisions
                } else {
                    KeyError::Duplicate(dups)
                }
                .into());
            }
            Outcome::Done => {
                let mut dups = pending.duplicates(&ra, &rb);
                for d in collect_dups(&mut shards) {
                    if dups.len() < FEW && !dups.contains(&d) {
                        dups.push(d);
                    }
                }
                if !dups.is_empty() {
                    return Err(KeyError::Duplicate(dups).into());
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
                pending.clear();
                for s in &mut shards {
                    pending.take_from(s)?;
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

/// The fullest shard table, as a share of its limit.
fn fill(shards: &[Pass<'_>]) -> f64 {
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

/// Threads for the comparison, and the refine-half bytes held for the
/// pool's own heap. Under a small budget one thread does it all, which
/// also keeps the pool out of a budget that could not absorb it.
fn threads(budget: &Budget) -> Result<(usize, Reservation)> {
    if budget.available(Half::Refine).0 < PARALLEL_REFINE {
        return Ok((1, budget.try_reserve(Half::Refine, Bytes(0))?));
    }
    let threads = rayon::current_num_threads().clamp(1, MAX_SHARDS);
    // The pool's queues and thread records: about 8 KiB per thread on
    // macOS, held twice over.
    let pool = budget.try_reserve(Half::Refine, Bytes(POOL_PER_THREAD * threads as u64))?;
    Ok((threads, pool))
}

/// How a read of both files ended.
enum Outcome {
    /// Both files were read to the end.
    Done,
    /// A shard ran out of the half named, this far through the files.
    Overflow(Half, f64),
    /// Enough duplicates, or too many suspects, to stop.
    Stop,
}

type Readers<'r, 's, A, B> = (
    &'r mut Reader<'s, A>,
    &'r mut Reader<'s, B>,
    &'r Mutex<Tables>,
);

/// Reads both files once, the shards taking the records in `lo..hi`, and
/// counts `pending` on the way. An empty range only counts.
fn read_pass<A, B>(
    (ra, rb, tables): &mut Readers<'_, '_, A, B>,
    shards: &mut [Pass<'_>],
    lo: u64,
    hi: u64,
    pending: &[Suspect],
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
    loop {
        let (ga, gb) = if n == 1 {
            (ra.advance(tables, &filter), rb.advance(tables, &filter))
        } else {
            rayon::join(
                || ra.advance(tables, &filter),
                || rb.advance(tables, &filter),
            )
        };
        let (ga, gb) = (ga?, gb?);
        if !ga && !gb {
            return Ok(Outcome::Done);
        }
        if lo == hi {
            continue;
        }
        let (ra, rb) = (&**ra, &**rb);
        if n == 1 {
            shards[0].round(0, 1, ra, rb);
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
        if let Some(half) = shards.iter().find_map(|s| s.overflow) {
            #[allow(clippy::cast_precision_loss, reason = "an estimate")]
            let progress = (ra.consumed() + rb.consumed()) as f64 / total.max(1) as f64;
            return Ok(Outcome::Overflow(half, progress));
        }
        let dups: usize = shards.iter().map(|s| s.dups.len()).sum();
        let suspects: usize = shards.iter().map(|s| s.suspects.len()).sum();
        if dups >= FEW || suspects >= SUSPECTS {
            return Ok(Outcome::Stop);
        }
    }
}

/// The waiting duplicates the shards found, the first few.
fn collect_dups(shards: &mut [Pass<'_>]) -> Vec<Vec<u8>> {
    let mut dups: Vec<Vec<u8>> = Vec::new();
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

    fn clear(&mut self) {
        for s in self.list.drain(..) {
            self.res.shrink(Bytes(s.key.len() as u64));
        }
    }

    /// Moves a shard's suspects here, as many as are counted at once.
    fn take_from(&mut self, shard: &mut Pass<'_>) -> Result<()> {
        for s in shard.suspects.drain(..) {
            let bytes = Bytes(s.key.len() as u64);
            shard.res.shrink(bytes);
            if self.list.len() < SUSPECTS {
                let budget = shard.budget;
                if !grow(&mut self.list, &mut self.res, budget, 1) {
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
struct Kept {
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
}

const _: () = assert!(size_of::<Kept>() == 48);

/// What a shard does after one record.
enum Flow {
    Next,
    Overflow(Half),
}

/// One shard of a pass, reused by the next pass.
#[derive(Debug)]
struct Pass<'b> {
    budget: &'b Budget,
    /// Open addressing, linear probing: `EMPTY`, a matched key's
    /// fingerprint, or `WAITING | index into kept`.
    slots: Vec<u32>,
    used: usize,
    limit: usize,
    /// Skeleton half: the slots.
    _table: Reservation,
    kept: Vec<Kept>,
    arena: Vec<u8>,
    /// Arena bytes and entries of `kept` no longer needed.
    garbage: usize,
    dead: usize,
    dups: Vec<Vec<u8>>,
    suspects: Vec<Suspect>,
    change: KeyedChange,
    /// Refine half: `kept`, the arena, the duplicates, the suspects and
    /// the key of `change`.
    res: Reservation,
    overflow: Option<Half>,
    fault: Option<Error>,
}

impl<'b> Pass<'b> {
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
            change: KeyedChange {
                key: Vec::new(),
                kind: ChangeKind::Changed,
                a: None,
                b: None,
            },
            res: budget.try_reserve(Half::Refine, Bytes(0))?,
            overflow: None,
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
        self.overflow = None;
        self.fault = None;
    }

    /// Takes this shard's records of the readers' last windows.
    fn round<A, B>(&mut self, k: usize, n: usize, ra: &Reader<'_, A>, rb: &Reader<'_, B>)
    where
        A: Source + ?Sized,
        B: Source + ?Sized,
    {
        let _ = self.feed(Side::A, k, n, ra.kept()) && self.feed(Side::B, k, n, rb.kept());
    }

    fn feed<'r>(
        &mut self,
        side: Side,
        k: usize,
        n: usize,
        recs: impl Iterator<Item = Record<'r>>,
    ) -> bool {
        for rec in recs.filter(|r| shard(r.hash, n) == k) {
            match self.take(side, &rec) {
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

    fn key(&self, k: &Kept) -> &[u8] {
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
    fn take(&mut self, side: Side, rec: &Record<'_>) -> Result<Flow> {
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
                        self.duplicate(key);
                    } else {
                        self.matched(index(v), i, side, rec);
                    }
                    return Ok(Flow::Next);
                }
            }
            i = self.next_slot(i);
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

    /// A key found waiting from the same file.
    fn duplicate(&mut self, key: &[u8]) {
        if self.dups.len() < FEW
            && !self.dups.iter().any(|d| d == key)
            && grow(&mut self.dups, &mut self.res, self.budget, 1)
            && self.res.grow(Bytes(key.len() as u64)).is_ok()
        {
            self.dups.push(key.to_vec());
        }
    }

    /// The other file's record for a waiting key: compare the bytes.
    fn matched(&mut self, at: usize, slot: usize, side: Side, rec: &Record<'_>) {
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
            && grow(&mut self.kept, &mut self.res, self.budget, 1)
            && grow(&mut self.arena, &mut self.res, self.budget, len)
            && grow(&mut self.change.key, &mut self.res, self.budget, key)
            && (!suspect
                || (grow(&mut self.suspects, &mut self.res, self.budget, 1)
                    && self.res.grow(Bytes(key as u64)).is_ok()));
        (!fits).then_some(Half::Refine)
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
        on_change: &mut impl FnMut(&KeyedChange) -> io::Result<()>,
    ) -> io::Result<()> {
        let k = self.kept[i];
        let change = &mut self.change;
        change.key.clear();
        change.key.extend_from_slice(
            &self.arena[k.start as usize + k.key_off as usize..][..k.key_len as usize],
        );
        (change.kind, change.a, change.b) = match k.state {
            State::A => (ChangeKind::Removed, Some(LineNo(k.line_a)), None),
            State::B => (ChangeKind::Added, None, Some(LineNo(k.line_b))),
            State::Changed | State::Dead => (
                ChangeKind::Changed,
                Some(LineNo(k.line_a)),
                Some(LineNo(k.line_b)),
            ),
        };
        on_change(change)
    }
}

/// Where a change sorts: by its line, in B for an added record.
fn order(k: &Kept) -> (u64, State) {
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
fn emit(
    shards: &mut [Pass<'_>],
    on_change: &mut impl FnMut(&KeyedChange) -> io::Result<()>,
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

    type Change = (ChangeKind, String, Option<u64>, Option<u64>);
    type Rows = Vec<(u32, u32)>;

    fn run(a: &[u8], b: &[u8], spec: &str, mem: u64) -> Result<(Vec<Change>, Report)> {
        let spec: KeySpec = spec.parse().unwrap();
        let mut out = Vec::new();
        let report = compare(a, b, &spec, &Budget::new(Bytes(mem)), |c| {
            out.push((
                c.kind,
                String::from_utf8_lossy(&c.key).into_owned(),
                c.a.map(|l| l.0),
                c.b.map(|l| l.0),
            ));
            Ok(())
        })?;
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
