//! Passes over both files, each owning one range of key hashes.
//!
//! A pass reads A and B side by side, at the same relative position, and
//! keeps only the records whose key hash falls in its range. A record
//! waits, with its bytes, until the same key turns up in the other file;
//! then the two are compared byte by byte and only the key hash stays, to
//! catch a third occurrence. Files in roughly the same order keep few
//! records waiting, so a pass costs about 8 bytes per key of its range.
//!
//! When the table or the waiting records outgrow the budget, the pass
//! gives up the top of its range and carries on: the records it drops come
//! back in a later pass. Nothing is ever written to disk.

use std::io;

use super::read::{Reader, Record, Tables};
use super::{ChangeKind, KeyError, KeyedChange, Side};
use crate::budget::{Budget, BudgetExceeded, Half, Reservation};
use crate::error::{Error, Result};
use crate::source::Source;
use crate::types::{Bytes, LineNo};

/// One past the largest key hash.
const END: u64 = 1 << 63;
/// A table slot holding the index of a waiting record, not a key hash.
const WAITING: u64 = 1 << 63;
const EMPTY: u64 = 0;
/// Duplicate keys listed in the error.
const FEW: usize = 5;
/// Possible duplicates collected before the pass stops to check them.
const SUSPECTS: usize = 64;
/// Read buffer per file, from the refine half.
const BUF: u64 = 1 << 20;

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
/// The read buffers and the waiting records come from the refine half,
/// the hash table from the skeleton half.
///
/// Time is O(passes × (|A| + |B|)); a pass holds about 8 bytes per key
/// of its range plus the records still waiting for their match.
///
/// # Errors
///
/// [`KeyError`] for duplicate or missing keys, [`Error::LineTooLong`] for
/// a record larger than the budget allows, a budget error when not even a
/// single key fits, an I/O error from a source or from `on_change`.
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
    let buf = BUF.min(budget.available(Half::Refine).0 / 8).max(64);
    let buf = usize::try_from(buf).unwrap_or(usize::MAX);
    let mut ra = Reader::new(a, Side::A, spec, budget, buf)?;
    let mut rb = Reader::new(b, Side::B, spec, budget, buf)?;
    let mut tables = Tables::new(budget)?;
    let (len_a, len_b) = (a.len().0, b.len().0);
    let mut pass = Pass::new(budget, ra.estimate_lines()? + rb.estimate_lines()?)?;
    let mut report = Report::default();
    let (mut lo, mut width) = (0, END);
    while lo < END {
        pass.start(lo, lo.saturating_add(width).min(END));
        ra.rewind();
        rb.rewind();
        let (mut done_a, mut done_b) = (false, false);
        let first = report.passes == 0;
        let stopped = loop {
            let (ca, cb) = (ra.consumed(), rb.consumed());
            let progress = if len_a + len_b == 0 {
                1.0
            } else {
                #[allow(clippy::cast_precision_loss, reason = "an estimate")]
                let p = (ca + cb) as f64 / (len_a + len_b) as f64;
                p
            };
            let take_a = !done_a
                && (done_b
                    || u128::from(ca) * u128::from(len_b) <= u128::from(cb) * u128::from(len_a));
            let flow = if take_a {
                step(&mut ra, &mut tables, &mut pass, progress)?
            } else if !done_b {
                step(&mut rb, &mut tables, &mut pass, progress)?
            } else {
                break false;
            };
            match flow {
                Flow::Next => {
                    if first {
                        if take_a {
                            report.records_a += 1;
                        } else {
                            report.records_b += 1;
                        }
                    }
                }
                Flow::End if take_a => done_a = true,
                Flow::End => done_b = true,
                Flow::Stop => break true,
            }
        };
        if !pass.suspects.is_empty() {
            confirm(&mut ra, &mut rb, &mut tables, &mut pass)?;
        }
        if !pass.dups.is_empty() {
            return Err(KeyError::Duplicate(std::mem::take(&mut pass.dups)).into());
        }
        if stopped {
            return Err(KeyError::Collisions.into());
        }
        if first
            && let super::KeySpec::Copy(name) = spec
            && ra.blocks() + rb.blocks() == 0
            && len_a + len_b > 0
        {
            return Err(KeyError::NoColumn {
                side: Side::A,
                name: name.clone(),
            }
            .into());
        }
        report.changes += pass.emit(&mut on_change)?;
        report.passes += 1;
        width = pass.hi - lo;
        lo = pass.hi;
    }
    Ok(report)
}

/// What the pass loop does after one read.
enum Flow {
    Next,
    End,
    Stop,
}

fn step<S: Source + ?Sized>(
    reader: &mut Reader<'_, S>,
    tables: &mut Tables,
    pass: &mut Pass<'_>,
    progress: f64,
) -> Result<Flow> {
    let side = reader.side();
    match reader.next(tables)? {
        None => Ok(Flow::End),
        Some(rec) => pass.take(side, &rec, progress),
    }
}

/// Reads both files again and keeps the suspects that occur twice in the
/// file they came from.
fn confirm<A, B>(
    ra: &mut Reader<'_, A>,
    rb: &mut Reader<'_, B>,
    tables: &mut Tables,
    pass: &mut Pass<'_>,
) -> Result<()>
where
    A: Source + ?Sized,
    B: Source + ?Sized,
{
    fn count<S: Source + ?Sized>(
        r: &mut Reader<'_, S>,
        tables: &mut Tables,
        suspects: &mut [Suspect],
    ) -> Result<()> {
        r.rewind();
        let side = r.side();
        while let Some(rec) = r.next(tables)? {
            for s in suspects.iter_mut().filter(|s| s.side == side) {
                if s.hash == rec.hash && s.table == rec.table && s.key == rec.bytes[rec.key.clone()]
                {
                    s.seen += 1;
                }
            }
        }
        Ok(())
    }
    count(ra, tables, &mut pass.suspects)?;
    count(rb, tables, &mut pass.suspects)?;
    for s in std::mem::take(&mut pass.suspects) {
        if s.seen > 1 && pass.dups.len() < FEW && !pass.dups.contains(&s.key) {
            pass.dups.push(s.key);
        } else {
            pass.res.shrink(Bytes(s.key.len() as u64));
        }
    }
    Ok(())
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
    /// Matched with equal bytes, or dropped with the top of the range.
    Dead,
}

/// A record kept by the pass, its bytes in the arena.
#[derive(Debug, Clone, Copy)]
struct Kept {
    hash: u64,
    line_a: u64,
    line_b: u64,
    /// Offset of the bytes in the arena.
    start: usize,
    /// Table slot pointing here, while waiting.
    slot: usize,
    len: u32,
    key_off: u32,
    key_len: u32,
    table: u32,
    state: State,
}

/// A key seen again after its match: a duplicate, or another key with the
/// same hash.
#[derive(Debug)]
struct Suspect {
    side: Side,
    table: u32,
    hash: u64,
    key: Vec<u8>,
    seen: u32,
}

/// State of one pass, reused by the next.
#[derive(Debug)]
struct Pass<'b> {
    budget: &'b Budget,
    lo: u64,
    hi: u64,
    /// Open addressing, linear probing: `EMPTY`, a matched key's hash, or
    /// `WAITING | index into kept`.
    slots: Vec<u64>,
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
}

impl<'b> Pass<'b> {
    /// A table for about `keys` keys, or as many as the skeleton half
    /// holds. Too small an estimate costs passes, not correctness.
    fn new(budget: &'b Budget, keys: u64) -> Result<Self> {
        let fits = budget.available(Half::Skeleton).0 / 8;
        let cap = fits.min(keys.saturating_mul(10) / 7 + 64);
        let table = budget.try_reserve(Half::Skeleton, Bytes(cap * 8))?;
        let cap = usize::try_from(cap).unwrap_or(usize::MAX);
        Ok(Self {
            budget,
            lo: 0,
            hi: END,
            slots: vec![EMPTY; cap],
            used: 0,
            // Linear probing slows down sharply above 70% load.
            limit: cap / 10 * 7,
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
        })
    }

    fn start(&mut self, lo: u64, hi: u64) {
        (self.lo, self.hi) = (lo, hi);
        self.slots.fill(EMPTY);
        self.used = 0;
        self.kept.clear();
        self.arena.clear();
        self.garbage = 0;
        self.dead = 0;
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

    fn hash_at(&self, v: u64) -> u64 {
        if v & WAITING == 0 {
            v
        } else {
            self.kept[index(v)].hash
        }
    }

    fn key(&self, k: &Kept) -> &[u8] {
        &self.arena[k.start + k.key_off as usize..][..k.key_len as usize]
    }

    /// Takes one record of `side`.
    fn take(&mut self, side: Side, rec: &Record<'_>, progress: f64) -> Result<Flow> {
        let waiting = match side {
            Side::A => State::A,
            Side::B => State::B,
        };
        let key = &rec.bytes[rec.key.clone()];
        loop {
            if rec.hash < self.lo || rec.hash >= self.hi {
                return Ok(Flow::Next);
            }
            let mut i = self.home(rec.hash);
            let mut seen = false;
            loop {
                let v = self.slots[i];
                if v == EMPTY {
                    break;
                }
                if v == rec.hash {
                    seen = true;
                } else if v & WAITING != 0 {
                    let k = self.kept[index(v)];
                    if k.hash == rec.hash && k.table == rec.table && self.key(&k) == key {
                        if k.state == waiting {
                            return Ok(self.duplicate(key));
                        }
                        self.matched(index(v), i, side, rec);
                        return Ok(Flow::Next);
                    }
                }
                i = self.next_slot(i);
            }
            if !self.room(rec.bytes.len(), key.len(), seen) {
                self.relieve(progress)?;
                continue;
            }
            let (Ok(len), Ok(key_off), Ok(key_len)) = (
                u32::try_from(rec.bytes.len()),
                u32::try_from(rec.key.start),
                u32::try_from(key.len()),
            ) else {
                return Err(Error::LineTooLong {
                    line: LineNo(rec.line),
                    cap: Bytes(u64::from(u32::MAX)),
                });
            };
            let at = self.kept.len();
            self.kept.push(Kept {
                hash: rec.hash,
                line_a: rec.line,
                line_b: rec.line,
                start: self.arena.len(),
                slot: i,
                len,
                key_off,
                key_len,
                table: rec.table,
                state: waiting,
            });
            self.arena.extend_from_slice(rec.bytes);
            self.slots[i] = WAITING | at as u64;
            self.used += 1;
            if seen {
                self.suspects.push(Suspect {
                    side,
                    table: rec.table,
                    hash: rec.hash,
                    key: key.to_vec(),
                    seen: 0,
                });
                if self.suspects.len() == SUSPECTS {
                    return Ok(Flow::Stop);
                }
            }
            return Ok(Flow::Next);
        }
    }

    /// A key found waiting from the same file.
    fn duplicate(&mut self, key: &[u8]) -> Flow {
        if self.dups.len() < FEW
            && !self.dups.iter().any(|d| d == key)
            && grow(&mut self.dups, &mut self.res, self.budget, 1)
            && self.res.grow(Bytes(key.len() as u64)).is_ok()
        {
            self.dups.push(key.to_vec());
        }
        if self.dups.len() >= FEW {
            Flow::Stop
        } else {
            Flow::Next
        }
    }

    /// The other file's record for a waiting key: compare the bytes.
    fn matched(&mut self, at: usize, slot: usize, side: Side, rec: &Record<'_>) {
        let k = &mut self.kept[at];
        let stored = &self.arena[k.start..][..k.len as usize];
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
        self.slots[slot] = k.hash;
    }

    /// Whether one more record of `len` bytes fits, with room to hand its
    /// `key` bytes out at the end of the pass and to keep them as a
    /// suspect; grows the lists from the refine half if needed.
    fn room(&mut self, len: usize, key: usize, suspect: bool) -> bool {
        if self.used >= self.limit {
            return false;
        }
        let full = self.kept.len() == self.kept.capacity()
            || self.arena.capacity() - self.arena.len() < len;
        if full && self.mostly_dead() {
            self.compact();
        }
        grow(&mut self.kept, &mut self.res, self.budget, 1)
            && grow(&mut self.arena, &mut self.res, self.budget, len)
            && grow(&mut self.change.key, &mut self.res, self.budget, key)
            && (!suspect
                || (grow(&mut self.suspects, &mut self.res, self.budget, 1)
                    && self.res.grow(Bytes(key as u64)).is_ok()))
    }

    /// Whether at least half of the kept bytes or entries are dead.
    fn mostly_dead(&self) -> bool {
        (self.garbage > 0 && 2 * self.garbage >= self.arena.len())
            || (self.dead > 0 && 2 * self.dead >= self.kept.len())
    }

    /// Frees room: compacts when at least half of the kept bytes or
    /// entries are dead, otherwise gives up the top of the range.
    fn relieve(&mut self, progress: f64) -> Result<()> {
        let table_full = self.used >= self.limit;
        if !table_full && self.mostly_dead() {
            self.compact();
            return Ok(());
        }
        let width = self.hi - self.lo;
        if width <= 1 {
            return Err(Error::Budget(BudgetExceeded {
                half: if table_full {
                    Half::Skeleton
                } else {
                    Half::Refine
                },
                requested: Bytes(8),
                available: self.budget.available(if table_full {
                    Half::Skeleton
                } else {
                    Half::Refine
                }),
            }));
        }
        // What was read so far filled the budget; keep the share of the
        // range that should fit to the end of the files, with a margin.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "an estimate, clamped below"
        )]
        let keep = (width as f64 * progress * 0.9) as u64;
        self.hi = self.lo + keep.clamp(1, width - 1);
        self.evict();
        self.compact();
        Ok(())
    }

    /// Drops every key at or above `hi` and resettles the table.
    fn evict(&mut self) {
        for i in 0..self.slots.len() {
            let v = self.slots[i];
            if v == EMPTY || self.hash_at(v) < self.hi {
                continue;
            }
            self.slots[i] = EMPTY;
            self.used -= 1;
            if v & WAITING != 0 {
                self.kept[index(v)].state = State::Dead;
            }
        }
        let hi = self.hi;
        for k in self.kept.iter_mut().filter(|k| k.hash >= hi) {
            k.state = State::Dead;
        }
        // Holes break probe chains. Reinsert every entry, walking from an
        // empty slot so each chain is rebuilt in order.
        let n = self.slots.len();
        let Some(empty) = self.slots.iter().position(|&v| v == EMPTY) else {
            return;
        };
        for step in 1..=n {
            let i = (empty + step) % n;
            let v = self.slots[i];
            if v == EMPTY {
                continue;
            }
            self.slots[i] = EMPTY;
            let mut j = self.home(self.hash_at(v));
            while self.slots[j] != EMPTY {
                j = self.next_slot(j);
            }
            self.slots[j] = v;
            if v & WAITING != 0 {
                self.kept[index(v)].slot = j;
            }
        }
    }

    /// Moves live entries and their bytes down over the dead ones.
    fn compact(&mut self) {
        let (mut w, mut aw) = (0, 0);
        for r in 0..self.kept.len() {
            let mut k = self.kept[r];
            let (from, n) = match k.state {
                State::Dead => continue,
                State::A | State::B => (k.start, k.len as usize),
                State::Changed => (k.start + k.key_off as usize, k.key_len as usize),
            };
            self.arena.copy_within(from..from + n, aw);
            k.start = aw;
            aw += n;
            if k.state == State::Changed {
                k.key_off = 0;
                k.len = k.key_len;
            } else {
                self.slots[k.slot] = WAITING | w as u64;
            }
            self.kept[w] = k;
            w += 1;
        }
        self.kept.truncate(w);
        self.arena.truncate(aw);
        self.garbage = 0;
        self.dead = 0;
    }

    /// Hands the pass's changes to `on_change`, ordered by line.
    fn emit(&mut self, on_change: &mut impl FnMut(&KeyedChange) -> io::Result<()>) -> Result<u64> {
        self.kept.retain(|k| k.state != State::Dead);
        self.kept.sort_unstable_by_key(|k| {
            (
                if k.state == State::B {
                    k.line_b
                } else {
                    k.line_a
                },
                k.state,
            )
        });
        for k in &self.kept {
            let change = &mut self.change;
            change.key.clear();
            change.key.extend_from_slice(
                &self.arena[k.start + k.key_off as usize..][..k.key_len as usize],
            );
            (change.kind, change.a, change.b) = match k.state {
                State::A => (ChangeKind::Removed, Some(LineNo(k.line_a)), None),
                State::B => (ChangeKind::Added, None, Some(LineNo(k.line_b))),
                State::Changed => (
                    ChangeKind::Changed,
                    Some(LineNo(k.line_a)),
                    Some(LineNo(k.line_b)),
                ),
                State::Dead => continue,
            };
            on_change(change)?;
        }
        Ok(self.kept.len() as u64)
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "indices into kept fit usize"
)]
fn index(v: u64) -> usize {
    (v & !WAITING) as usize
}

/// Makes room for `need` more elements in `v`, paid from `res`: double
/// the capacity when the half allows, else exactly `need`.
fn grow<T>(v: &mut Vec<T>, res: &mut Reservation, budget: &Budget, need: usize) -> bool {
    if v.capacity() - v.len() >= need {
        return true;
    }
    let size = size_of::<T>().max(1);
    let free = usize::try_from(budget.available(Half::Refine).0).unwrap_or(usize::MAX) / size;
    let missing = need - (v.capacity() - v.len());
    let add = missing.max(v.capacity().max(64).min(free));
    if res.grow(Bytes((add * size) as u64)).is_err() {
        return false;
    }
    if v.try_reserve_exact(v.capacity() + add - v.len()).is_err() {
        res.shrink(Bytes((add * size) as u64));
        return false;
    }
    true
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
        let (a, b) = pair(3000, 7);
        let want = expected(&a, &b);
        assert!(want.len() > 50);
        let (ca, cb) = (csv(&a), csv(&b));
        let (one, report) = run(&ca, &cb, "csv:1", 1 << 22).unwrap();
        assert_eq!(report.passes, 1);
        assert_eq!(one, want);
        for mem in [24 << 10, 64 << 10] {
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
        fn any_budget_finds_the_hash_map_answer(n in 1u32..1500, seed in any::<u64>(), mem in 16u64..256) {
            let (a, b) = pair(n, seed);
            let (got, _) = run(&csv(&a), &csv(&b), "csv:1", mem << 10).unwrap();
            prop_assert_eq!(got, expected(&a, &b));
        }
    }
}
