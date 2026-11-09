//! Records and their keys, read from a [`Source`] one window at a time and
//! split into records on several threads.
//!
//! A record is one line, except in CSV, where a quoted field can hold line
//! breaks. Its bytes leave out the line terminator (`\n` or `\r\n`), so a
//! change of line ending alone is not a change of the record. Blank lines
//! are not records, and in a `pg_dump` only the rows of `COPY` blocks that
//! have the key column are.
//!
//! A window is cut into chunks at line breaks, and every chunk is parsed
//! on its own thread as if a record started at its first byte, in the
//! state the window started in. A chunk counts only when the one before
//! it ended where it starts, in the state it assumed; otherwise the next
//! round parses again from there. A quoted line break at a cut, or a
//! `COPY` header or terminator, costs a round; plain rows never do.
//!
//! A `COPY` table gets its id when it is first named, so only the first
//! chunk of a round, which starts where a sequential read would be, may
//! name a new table. Any other chunk stops at a header of a table not yet
//! named: it may be a guess from the wrong state, and its id depends on
//! the tables named before it.

use std::io;
use std::ops::Range;
use std::sync::{Mutex, PoisonError};

use memchr::{memchr, memchr2, memchr3};
use rayon::prelude::*;
use xxhash_rust::xxh3::{xxh3_64, xxh3_64_with_seed};

use super::{CsvColumn, KeyError, KeySpec, Side};
use crate::budget::{Budget, BudgetExceeded, Half, Reservation};
use crate::error::{Error, Result};
use crate::source::Source;
use crate::types::{ByteOffset, Bytes, LineNo};

/// Fewest bytes in a chunk; a smaller window is parsed by one thread.
// Fuzz builds cut small windows into chunks too, as the tests do.
const MIN_CHUNK: usize = if cfg!(test) || cfg!(fuzzing) {
    8
} else {
    64 << 10
};
/// Suspects one read counts at most.
pub const SUSPECTS: usize = 64;

/// One record in a reader's window.
#[derive(Debug)]
pub struct Record<'b> {
    /// The record without its line terminator.
    pub bytes: &'b [u8],
    /// Where the key is in `bytes`.
    pub key: Range<usize>,
    /// The record's first line.
    pub line: u64,
    /// The `COPY` block's table, 0 outside `pg_dump` files.
    pub table: u32,
    /// The key hash, see [`key_hash`].
    pub hash: u64,
}

/// A 63-bit key hash that is never 0, so a table slot can use 0 for empty
/// and the top bit for its own flag. Keys of different `COPY` tables hash
/// apart through the table's seed.
#[must_use]
pub fn key_hash(key: &[u8], seed: u64) -> u64 {
    (xxh3_64_with_seed(key, seed) >> 1) | 1
}

/// Names of the `COPY` tables seen so far, shared by both files so a name
/// gets the same id in each.
#[derive(Debug)]
pub struct Tables {
    /// Each name, its hash seed, and whether A and B have paid for it.
    names: Vec<(Box<[u8]>, u64, [bool; 2])>,
    /// Refine half: the names and the list.
    res: Reservation,
}

impl Tables {
    /// No names yet; the list's memory comes from the refine half.
    ///
    /// # Errors
    ///
    /// A budget error from the reservation.
    pub fn new(budget: &Budget) -> Result<Self> {
        Ok(Self {
            names: Vec::new(),
            res: budget.try_reserve(Half::Refine, Bytes(0))?,
        })
    }

    /// The id and hash seed of a table name, if `side` named it already.
    /// A name the other file gave counts only once this one gives it too,
    /// so where a chunk stops does not depend on the other reader's pace.
    fn find(&self, name: &[u8], side: Side) -> Option<(u32, u64)> {
        let i = self
            .names
            .iter()
            .position(|(n, _, paid)| **n == *name && paid[side as usize])?;
        Some((id(i), self.names[i].1))
    }

    /// The id and hash seed of a table name, added if new, paid from
    /// `credit`.
    ///
    /// A and B can name the same table at once, so each side pays for a
    /// name the first time it asks: what a side pays then depends only on
    /// its own file, not on which side asked first.
    fn id(
        &mut self,
        name: &[u8],
        side: Side,
        budget: &Budget,
        credit: &mut u64,
    ) -> Result<(u32, u64)> {
        let i = match self.names.iter().position(|(n, ..)| **n == *name) {
            Some(i) if self.names[i].2[side as usize] => return Ok((id(i), self.names[i].1)),
            Some(i) => i,
            None => self.names.len(),
        };
        // The list doubles, so two slots per name cover its capacity.
        let slot = size_of::<(Box<[u8]>, u64, [bool; 2])>() as u64;
        let cost = name.len() as u64 + 2 * slot;
        if !spend(&mut self.res, credit, cost) {
            return Err(Error::Budget(BudgetExceeded {
                half: Half::Refine,
                requested: Bytes(cost),
                available: budget.available(Half::Refine),
            }));
        }
        if i == self.names.len() {
            self.names.push((name.into(), xxh3_64(name), [false; 2]));
        }
        self.names[i].2[side as usize] = true;
        Ok((id(i), self.names[i].1))
    }
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a dump has far fewer than 2^32 tables"
)]
fn id(i: usize) -> u32 {
    i as u32 + 1
}

/// A key seen again after its match: a duplicate, or another key whose
/// hash shares the matched key's table slot fingerprint. A later read of
/// its file counts it.
#[derive(Debug)]
pub struct Suspect {
    /// The file it was seen in.
    pub side: Side,
    /// Its `COPY` table.
    pub table: u32,
    /// Its key hash.
    pub hash: u64,
    /// Its key.
    pub key: Vec<u8>,
}

/// Which records a read keeps, by key hash, and which suspects it counts.
#[derive(Debug)]
pub struct Filter<'p> {
    lo: u64,
    hi: u64,
    suspects: &'p [Suspect],
    /// One bit per suspect hash, to skip the list for almost every record.
    mask: u64,
}

impl<'p> Filter<'p> {
    /// Keeps hashes in `lo..hi` and counts the first [`SUSPECTS`] of
    /// `suspects`.
    #[must_use]
    pub fn new(lo: u64, hi: u64, suspects: &'p [Suspect]) -> Self {
        let suspects = &suspects[..suspects.len().min(SUSPECTS)];
        let mask = suspects.iter().fold(0, |m, s| m | bit(s.hash));
        Self {
            lo,
            hi,
            suspects,
            mask,
        }
    }

    fn count(&self, side: Side, table: u32, hash: u64, key: &[u8], seen: &mut [u32; SUSPECTS]) {
        if self.mask & bit(hash) == 0 {
            return;
        }
        for (s, n) in self.suspects.iter().zip(seen) {
            if s.side == side && s.hash == hash && s.table == table && s.key == key {
                *n = n.saturating_add(1);
            }
        }
    }
}

fn bit(hash: u64) -> u64 {
    1 << ((hash >> 1) & 63)
}

/// A record kept from a window, as offsets into it.
#[derive(Debug, Clone, Copy)]
struct Rec {
    hash: u64,
    off: u32,
    len: u32,
    key_off: u32,
    key_len: u32,
    /// Lines from the chunk's first.
    line: u32,
    table: u32,
}

const _: () = assert!(size_of::<Rec>() == 32);

/// What the reader knows about the file's layout.
#[derive(Debug)]
enum Kind {
    /// CSV, keyed by the column the header names, or by position.
    Csv { name: Option<Box<str>> },
    /// NDJSON with a field path.
    Ndjson(Box<[Box<str>]>),
    /// `pg_dump` text, keyed by a `COPY` column.
    Copy { name: Box<str> },
}

/// Where a `pg_dump` reader is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    /// Outside any `COPY` block.
    Outside,
    /// In a block; its rows are records when the block has the column,
    /// with that column, the table's id and its hash seed.
    Rows(Option<(u32, u32, u64)>),
}

/// The parser's state at a record boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct State {
    /// The CSV key column, once known.
    col: Option<u32>,
    block: Block,
}

/// Why a chunk's parse ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    /// It reached the chunk's end.
    Done,
    /// A record runs past the bytes read so far.
    Short,
    /// Its list could not grow.
    Full,
    /// A record has no key, or a read failed.
    Failed,
    /// A `COPY` header names a new table, which only the first chunk of a
    /// round may do.
    NewTable,
}

/// The error a chunk stopped on, with the line counted from its first.
#[derive(Debug)]
enum Fault {
    NoKey(u64),
    Other(Error),
}

/// Why [`Ctx::key_of`] found no key for a line that needs one.
#[derive(Debug)]
enum Miss {
    /// The record has no key.
    NoKey,
    /// A `COPY` header names a table this chunk may not name.
    NewTable,
    /// The header lacks the named column, or a budget error.
    Error(Error),
}

/// One chunk of a window: where it starts and ends and the records it
/// keeps.
#[derive(Debug)]
struct Chunk {
    recs: Vec<Rec>,
    /// Refine half: `recs`.
    res: Reservation,
    /// Refine-half bytes `recs` may still grow by this round, see
    /// [`grow`].
    credit: u64,
    start: usize,
    until: usize,
    end: usize,
    state0: State,
    state1: State,
    lines: u64,
    /// File line of its first byte, once it counts.
    line0: u64,
    records: u64,
    blocks: u64,
    seen: [u32; SUSPECTS],
    stop: Stop,
    fault: Option<Fault>,
}

impl Chunk {
    /// Splits `buf[self.start..]` into records up to `self.until`, keeping
    /// those `filter` keeps. A record that starts before `until` is read
    /// whole, past it if need be. Only a chunk that `may_name` tables
    /// gives a new `COPY` table its id.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the window stays below 4 GiB, see Reader::refill"
    )]
    fn parse(&mut self, buf: &[u8], eof: bool, may_name: bool, ctx: &Ctx<'_>) {
        self.recs.clear();
        (self.lines, self.records, self.blocks) = (0, 0, 0);
        self.seen = [0; SUSPECTS];
        self.fault = None;
        self.stop = Stop::Done;
        let mut state = self.state0;
        let mut pos = self.start;
        while pos < self.until {
            if self.recs.len() == self.recs.capacity()
                && !grow(
                    &mut self.recs,
                    &mut self.res,
                    ctx.budget,
                    &mut self.credit,
                    1,
                )
            {
                self.stop = Stop::Full;
                break;
            }
            let avail = &buf[pos..];
            let split = match ctx.kind {
                Kind::Csv { .. } => csv_record(avail, state.col.unwrap_or(u32::MAX), eof),
                _ => line_record(avail, eof),
            };
            let Some(split) = split else {
                self.stop = Stop::Short;
                break;
            };
            let rec = &avail[..split.len];
            let line = self.lines;
            let found = if rec.is_empty() {
                Ok(None)
            } else {
                ctx.key_of(
                    &mut state,
                    rec,
                    split.key,
                    may_name,
                    &mut self.blocks,
                    &mut self.credit,
                )
            };
            let found = match found {
                Ok(found) => found,
                Err(Miss::NewTable) => {
                    self.stop = Stop::NewTable;
                    break;
                }
                Err(Miss::NoKey) => {
                    self.fault = Some(Fault::NoKey(line));
                    self.stop = Stop::Failed;
                    break;
                }
                Err(Miss::Error(e)) => {
                    self.fault = Some(Fault::Other(e));
                    self.stop = Stop::Failed;
                    break;
                }
            };
            let at = pos;
            pos += split.next;
            self.lines += split.lines;
            let Some((key, table, seed)) = found else {
                continue;
            };
            self.records += 1;
            let hash = key_hash(&rec[key.clone()], seed);
            let filter = ctx.filter;
            filter.count(ctx.side, table, hash, &rec[key.clone()], &mut self.seen);
            if hash >= filter.lo && hash < filter.hi {
                self.recs.push(Rec {
                    hash,
                    off: at as u32,
                    len: split.len as u32,
                    key_off: key.start as u32,
                    key_len: key.len() as u32,
                    line: line as u32,
                    table,
                });
            }
        }
        self.end = pos;
        self.state1 = state;
    }
}

/// A record's key, its table and the table's hash seed; `None` for a line
/// that is not a record.
type Found = Option<(Range<usize>, u32, u64)>;

/// What every chunk of a window shares while it is parsed.
struct Ctx<'c> {
    kind: &'c Kind,
    side: Side,
    tables: &'c Mutex<Tables>,
    filter: &'c Filter<'c>,
    budget: &'c Budget,
}

impl Ctx<'_> {
    /// The key, table and seed of a record, `None` for a line that is not
    /// a record. A `COPY` header of a table not yet named is
    /// [`Miss::NewTable`] unless `may_name`, and leaves `state` as it was.
    #[allow(
        clippy::inline_always,
        reason = "called for every record; out of line once it took the credit, it slowed Chunk::parse by 1.4% on a 64 MiB SQL pair"
    )]
    #[inline(always)]
    fn key_of(
        &self,
        state: &mut State,
        rec: &[u8],
        csv_key: Option<Range<usize>>,
        may_name: bool,
        blocks: &mut u64,
        credit: &mut u64,
    ) -> std::result::Result<Found, Miss> {
        match self.kind {
            Kind::Csv { name } => {
                if state.col.is_some() {
                    return csv_key.map(|k| Some((k, 0, 0))).ok_or(Miss::NoKey);
                }
                let name = name.as_deref().unwrap_or_default();
                let found = (0..u32::MAX)
                    .map_while(|i| csv_field(rec, i).map(|r| (i, r)))
                    .find(|(_, r)| unquoted_eq(&rec[r.clone()], name.as_bytes()));
                let Some((i, key)) = found else {
                    return Err(Miss::Error(Error::Keyed(KeyError::NoColumn {
                        side: self.side,
                        name: name.into(),
                    })));
                };
                state.col = Some(i);
                Ok(Some((key, 0, 0)))
            }
            Kind::Ndjson(path) => json_key(rec, path)
                .map(|k| Some((k, 0, 0)))
                .ok_or(Miss::NoKey),
            Kind::Copy { name } => match state.block {
                Block::Outside => {
                    if let Some((table, cols)) = copy_header(rec) {
                        state.block = Block::Rows(match copy_column(cols, name.as_bytes()) {
                            Some(col) => {
                                let mut tables =
                                    self.tables.lock().unwrap_or_else(PoisonError::into_inner);
                                let (id, seed) = if may_name {
                                    tables
                                        .id(table, self.side, self.budget, credit)
                                        .map_err(Miss::Error)?
                                } else {
                                    tables.find(table, self.side).ok_or(Miss::NewTable)?
                                };
                                *blocks += 1;
                                Some((col, id, seed))
                            }
                            None => None,
                        });
                    }
                    Ok(None)
                }
                Block::Rows(_) if rec == b"\\." => {
                    state.block = Block::Outside;
                    Ok(None)
                }
                Block::Rows(None) => Ok(None),
                Block::Rows(Some((col, table, seed))) => tab_field(rec, col)
                    .map(|k| Some((k, table, seed)))
                    .ok_or(Miss::NoKey),
            },
        }
    }
}

/// Reads one file's records, front to back, any number of times.
#[derive(Debug)]
pub struct Reader<'s, S: ?Sized> {
    src: &'s S,
    side: Side,
    kind: Kind,
    /// At `buf[start]`.
    state: State,
    buf: Vec<u8>,
    /// Refine half: the buffer and the chunk list.
    res: Reservation,
    budget: Budget,
    start: usize,
    end: usize,
    /// File offset of `buf[end]`.
    pos: u64,
    /// Line of `buf[start]`.
    line: u64,
    eof: bool,
    /// `COPY` blocks seen with the key column, and records, since the
    /// last rewind.
    blocks: u64,
    records: u64,
    chunks: Vec<Chunk>,
    /// Chunks of the last round that count.
    valid: usize,
    seen: [u32; SUSPECTS],
    /// The buffer could not grow to hold one record.
    oversized: bool,
    /// Credit of each chunk's list per round, see [`grow`].
    share: u64,
}

/// Refine-half bytes a reader holds for a `buf`-byte window cut into
/// `chunks` chunks, before its record lists grow.
pub(crate) fn reader_bytes(buf: usize, chunks: usize) -> u64 {
    (buf + chunks.max(1) * size_of::<Chunk>()) as u64
}

impl<'s, S: Source + ?Sized> Reader<'s, S> {
    /// A reader with a `buf`-byte window cut into at most `chunks`
    /// chunks, from the refine half.
    ///
    /// # Errors
    ///
    /// A budget error if the refine half cannot hold the window.
    pub fn new(
        src: &'s S,
        side: Side,
        spec: &KeySpec,
        budget: &Budget,
        buf: usize,
        chunks: usize,
    ) -> Result<Self> {
        let chunks = chunks.max(1);
        let res = budget.try_reserve(Half::Refine, Bytes(reader_bytes(buf, chunks)))?;
        let (kind, col) = match spec {
            KeySpec::Csv(CsvColumn::Index(i)) => (Kind::Csv { name: None }, Some(*i)),
            KeySpec::Csv(CsvColumn::Header(name)) => (
                Kind::Csv {
                    name: Some(name.as_str().into()),
                },
                None,
            ),
            KeySpec::Ndjson(path) => (
                Kind::Ndjson(path.iter().map(|s| s.as_str().into()).collect()),
                None,
            ),
            KeySpec::Copy(name) => (
                Kind::Copy {
                    name: name.as_str().into(),
                },
                None,
            ),
        };
        let state = State {
            col,
            block: Block::Outside,
        };
        let mut list = Vec::with_capacity(chunks);
        for _ in 0..chunks {
            list.push(Chunk {
                recs: Vec::new(),
                res: budget.try_reserve(Half::Refine, Bytes(0))?,
                credit: u64::MAX,
                start: 0,
                until: 0,
                end: 0,
                state0: state,
                state1: state,
                lines: 0,
                line0: 0,
                records: 0,
                blocks: 0,
                seen: [0; SUSPECTS],
                stop: Stop::Done,
                fault: None,
            });
        }
        Ok(Self {
            src,
            side,
            kind,
            state,
            buf: vec![0; buf],
            res,
            budget: budget.clone(),
            start: 0,
            end: 0,
            pos: 0,
            line: 0,
            eof: false,
            blocks: 0,
            records: 0,
            chunks: list,
            valid: 0,
            seen: [0; SUSPECTS],
            oversized: false,
            share: u64::MAX,
        })
    }

    /// Goes back to the start of the file. A CSV column found in the
    /// header stays found.
    pub fn rewind(&mut self) {
        (self.start, self.end, self.pos, self.line) = (0, 0, 0, 0);
        self.eof = false;
        (self.blocks, self.records, self.valid) = (0, 0, 0);
        self.seen = [0; SUSPECTS];
        self.state.block = Block::Outside;
    }

    /// Lines in the file, extrapolated from the first window. Reads it, so
    /// call before the first [`advance`](Self::advance).
    pub(crate) fn estimate_lines(&mut self) -> Result<u64> {
        self.refill()?;
        let lines = memchr::memchr_iter(b'\n', &self.buf[..self.end]).count() as u64 + 1;
        let read = (self.end as u64).max(1);
        Ok(
            u64::try_from(u128::from(self.src.len().0) * u128::from(lines) / u128::from(read))
                .unwrap_or(u64::MAX)
                .max(lines),
        )
    }

    /// Frees the record lists, which keep their largest size otherwise;
    /// for a pass that starts over on a narrower range.
    pub(crate) fn shrink_lists(&mut self) {
        for c in &mut self.chunks {
            c.res
                .shrink(Bytes((c.recs.capacity() * size_of::<Rec>()) as u64));
            c.recs = Vec::new();
        }
    }

    /// Gives each chunk's list `share` bytes of credit per round, for a
    /// reader that runs beside another; see [`grow`].
    pub(crate) fn set_share(&mut self, share: u64) {
        self.share = share;
    }

    /// Bytes in the file.
    pub(crate) fn len(&self) -> u64 {
        self.src.len().0
    }

    /// Bytes of the file read and split into records so far.
    #[must_use]
    pub fn consumed(&self) -> u64 {
        self.pos - (self.end - self.start) as u64
    }

    /// `COPY` blocks with the key column since the last rewind.
    #[must_use]
    pub fn blocks(&self) -> u64 {
        self.blocks
    }

    /// Records since the last rewind.
    #[must_use]
    pub fn records(&self) -> u64 {
        self.records
    }

    /// How often each suspect of this file was seen since the last rewind.
    pub(crate) fn seen(&self) -> &[u32; SUSPECTS] {
        &self.seen
    }

    /// The records the last [`advance`](Self::advance) kept, in file
    /// order.
    pub fn kept(&self) -> impl Iterator<Item = Record<'_>> {
        let buf = &self.buf;
        self.chunks[..self.valid].iter().flat_map(move |c| {
            c.recs.iter().map(move |r| {
                let at = r.off as usize;
                let bytes = &buf[at..at + r.len as usize];
                Record {
                    bytes,
                    key: r.key_off as usize..(r.key_off + r.key_len) as usize,
                    line: c.line0 + u64::from(r.line),
                    table: r.table,
                    hash: r.hash,
                }
            })
        })
    }

    /// File offset of a record that [`kept`](Self::kept) handed out from
    /// the last window, found from where its bytes lie in the window so
    /// that only a caller that asks pays for it.
    pub(crate) fn offset(&self, rec: &Record<'_>) -> u64 {
        let at = rec.bytes.as_ptr() as usize - self.buf.as_ptr() as usize;
        debug_assert!(at + rec.bytes.len() <= self.end, "a record of this window");
        self.pos - self.end as u64 + at as u64
    }

    /// Reads and splits the next window, keeping the records `filter`
    /// keeps; `false` at the end of the file.
    ///
    /// # Errors
    ///
    /// A record without the key, a CSV header without the named column, a
    /// record that does not fit in the buffer, or a read error.
    pub fn advance(&mut self, tables: &Mutex<Tables>, filter: &Filter<'_>) -> Result<bool> {
        self.valid = 0;
        if !self.eof {
            self.refill()?;
        }
        if self.start == self.end && self.eof {
            return Ok(false);
        }
        let (base, end) = (self.start, self.end);
        let n = if matches!(self.kind, Kind::Csv { .. }) && self.state.col.is_none() {
            1
        } else {
            ((end - base) / MIN_CHUNK).clamp(1, self.chunks.len())
        };
        let size = (end - base).div_ceil(n);
        let mut k = 0;
        let mut at = base;
        while k < n && at < end {
            let nominal = base + (k + 1) * size;
            let next = if nominal >= end {
                end
            } else {
                memchr(b'\n', &self.buf[nominal..end]).map_or(end, |i| nominal + i + 1)
            };
            let c = &mut self.chunks[k];
            (c.start, c.until, c.state0) = (at, next, self.state);
            c.credit = self.share;
            at = next;
            k += 1;
        }
        let ctx = Ctx {
            kind: &self.kind,
            side: self.side,
            tables,
            filter,
            budget: &self.budget,
        };
        let (buf, eof) = (&self.buf[..end], self.eof);
        if k == 1 {
            self.chunks[0].parse(buf, eof, true, &ctx);
        } else {
            self.chunks[..k]
                .par_iter_mut()
                .enumerate()
                .for_each(|(i, c)| c.parse(buf, eof, i == 0, &ctx));
        }
        for i in 0..k {
            if i > 0 {
                let (p, c) = (&self.chunks[i - 1], &self.chunks[i]);
                if p.stop != Stop::Done || p.end != c.start || p.state1 != c.state0 {
                    break;
                }
            }
            let c = &mut self.chunks[i];
            c.line0 = self.line;
            if let Some(fault) = c.fault.take() {
                return Err(match fault {
                    Fault::NoKey(l) => Error::Keyed(KeyError::NoKey {
                        side: self.side,
                        line: LineNo(self.line + l),
                    }),
                    Fault::Other(e) => e,
                });
            }
            self.line += c.lines;
            self.records += c.records;
            self.blocks += c.blocks;
            for (s, n) in self.seen.iter_mut().zip(c.seen) {
                *s = s.saturating_add(n);
            }
            self.start = c.end;
            self.state = c.state1;
            self.valid = i + 1;
        }
        if self.start == base && self.chunks[0].stop == Stop::Full {
            return Err(Error::Budget(BudgetExceeded {
                half: Half::Refine,
                requested: Bytes(size_of::<Rec>() as u64),
                available: self.budget.available(Half::Refine),
            }));
        }
        Ok(true)
    }

    /// Moves the unread bytes to the front and reads until the buffer is
    /// full, growing it when one record fills it.
    fn refill(&mut self) -> Result<()> {
        self.make_room()?;
        while self.end < self.buf.len() {
            match self
                .src
                .read_at(&mut self.buf[self.end..], ByteOffset(self.pos))
            {
                Ok(0) => {
                    self.eof = true;
                    break;
                }
                Ok(n) => {
                    self.end += n;
                    self.pos += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
        Ok(())
    }

    /// The first half of [`refill`](Self::refill): moves the unread bytes
    /// to the front and doubles the buffer when one record fills it. A
    /// second call before the read changes nothing, so a caller can grow
    /// both windows one after the other before they are read side by side,
    /// and neither growth depends on the other's lists. The buffer stays
    /// below 4 GiB so a record's offsets fit `u32`.
    ///
    /// # Errors
    ///
    /// A record too long for key mode, or a budget error.
    pub(crate) fn make_room(&mut self) -> Result<()> {
        if self.eof {
            return Ok(());
        }
        if self.start > 0 {
            self.buf.copy_within(self.start..self.end, 0);
            self.end -= self.start;
            self.start = 0;
        }
        if self.end == self.buf.len() {
            let more = self.buf.len();
            if self.buf.len() + more > u32::MAX as usize {
                return Err(Error::Keyed(KeyError::TooLong {
                    side: self.side,
                    line: LineNo(self.line),
                    cap: Bytes(self.buf.len() as u64),
                }));
            }
            if let Err(e) = self.res.grow(Bytes(more as u64)) {
                self.oversized = true;
                return Err(e.into());
            }
            if self.buf.try_reserve_exact(more).is_err() {
                self.res.shrink(Bytes(more as u64));
                return Err(io::Error::from(io::ErrorKind::OutOfMemory).into());
            }
            self.buf.resize(self.buf.len() + more, 0);
        }
        Ok(())
    }
}

impl<S: Source + ?Sized> Reader<'_, S> {
    /// Bytes the window holds now, chunks included.
    pub(crate) fn held(&self) -> u64 {
        reader_bytes(self.buf.len(), self.chunks.len())
    }

    /// Whether the window could not grow to hold the record at its front.
    pub(crate) fn oversized(&self) -> bool {
        self.oversized
    }

    /// Bytes the window has grown to beyond `share`, its size when the
    /// reader was made: it held a record that long, or half as long.
    pub(crate) fn grown(&self, share: usize) -> Option<u64> {
        (self.buf.len() > share).then_some(self.buf.len() as u64)
    }

    /// The bytes the record at the window's front takes, terminator
    /// included, found by reading on to its end.
    ///
    /// Reads through a stack buffer and allocates nothing; only the --mem
    /// advice of a refusal calls it.
    ///
    /// # Errors
    ///
    /// A read error.
    #[cold]
    pub(crate) fn front_record(&self) -> Result<u64> {
        // The record ends at the first line break outside quotes, as
        // `csv_record` and `line_record` end it; at the end of the file it
        // takes the rest.
        let csv = matches!(self.kind, Kind::Csv { .. });
        let mut quoted = false;
        let mut end_in = |bytes: &[u8]| {
            let mut i = 0;
            loop {
                let hit = if csv {
                    memchr2(b'"', b'\n', &bytes[i..])
                } else {
                    memchr(b'\n', &bytes[i..])
                };
                let j = i + hit?;
                if bytes[j] == b'"' {
                    quoted = !quoted;
                } else if !quoted {
                    return Some(j + 1);
                }
                i = j + 1;
            }
        };
        let held = &self.buf[self.start..self.end];
        if let Some(n) = end_in(held) {
            return Ok(n as u64);
        }
        if self.eof {
            return Ok(held.len() as u64);
        }
        let mut len = held.len() as u64;
        let mut chunk = [0u8; 16 << 10];
        loop {
            match self
                .src
                .read_at(&mut chunk, ByteOffset(self.pos + len - held.len() as u64))
            {
                Ok(0) => return Ok(len),
                Ok(n) => {
                    if let Some(end) = end_in(&chunk[..n]) {
                        return Ok(len + end as u64);
                    }
                    len += n as u64;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e.into()),
            }
        }
    }
}

/// Makes room for `need` more elements in `v`, paid from `res` and
/// `credit`: double the capacity when half of what is free allows, so the
/// other lists keep room to grow, else exactly `need`. What is free is the
/// smaller of `credit` and what the refine half has free.
///
/// Lists that grow at the same time on several threads each get a credit,
/// their share of the half as it was before they started, so whether one
/// can grow depends on its own records only and not on what the others
/// took first. A list that grows alone takes `u64::MAX`.
// Out of line: a list grows a few times per pass, and inlined into
// `Chunk::parse` it slowed the per-record loop.
#[inline(never)]
pub(crate) fn grow<T>(
    v: &mut Vec<T>,
    res: &mut Reservation,
    budget: &Budget,
    credit: &mut u64,
    need: usize,
) -> bool {
    if v.capacity() - v.len() >= need {
        return true;
    }
    let size = size_of::<T>().max(1);
    let free = budget.available(Half::Refine).0.min(*credit);
    let free = usize::try_from(free).unwrap_or(usize::MAX) / size;
    let missing = need - (v.capacity() - v.len());
    let add = missing.max(v.capacity().max(64).min(free / 2));
    let bytes = (add * size) as u64;
    if !spend(res, credit, bytes) {
        return false;
    }
    if v.try_reserve_exact(v.capacity() + add - v.len()).is_err() {
        res.shrink(Bytes(bytes));
        *credit += bytes;
        return false;
    }
    true
}

/// Moves `bytes` of `credit` into `res`, if both have them.
pub(crate) fn spend(res: &mut Reservation, credit: &mut u64, bytes: u64) -> bool {
    if bytes > *credit || res.grow(Bytes(bytes)).is_err() {
        return false;
    }
    *credit -= bytes;
    true
}

/// A record split off the front of the buffer, as offsets into it.
#[derive(Debug, PartialEq, Eq)]
struct Split {
    /// Record length without the terminator.
    len: usize,
    /// Bytes consumed, terminator included.
    next: usize,
    /// Line breaks consumed.
    lines: u64,
    /// The key column, if the record has it (CSV only).
    key: Option<Range<usize>>,
}

/// The line at the front of `buf`, or `None` if it may go on past it.
fn line_record(buf: &[u8], eof: bool) -> Option<Split> {
    let (end, next) = match memchr(b'\n', buf) {
        Some(nl) => (nl, nl + 1),
        None if eof => (buf.len(), buf.len()),
        None => return None,
    };
    Some(Split {
        len: trim_cr(buf, end),
        next,
        lines: 1,
        key: None,
    })
}

fn trim_cr(buf: &[u8], end: usize) -> usize {
    if end > 0 && buf[end - 1] == b'\r' {
        end - 1
    } else {
        end
    }
}

/// The CSV record at the front of `buf` (RFC 4180) and its field `col`, or
/// `None` if the record may go on past `buf`.
///
/// Quote parity decides what a comma or line break means: inside quotes
/// it is data. A doubled quote flips the parity twice, so it needs no case
/// of its own, and a stray quote in an unquoted field is read the same
/// lenient way.
fn csv_record(buf: &[u8], col: u32, eof: bool) -> Option<Split> {
    let (mut field, mut field_start, mut quoted) = (0u32, 0, false);
    let (mut key, mut lines, mut i) = (None, 0, 0);
    let (end, next) = loop {
        let hit = if key.is_some() {
            memchr2(b'"', b'\n', &buf[i..])
        } else {
            memchr3(b',', b'"', b'\n', &buf[i..])
        };
        let Some(j) = hit.map(|off| i + off) else {
            if !eof {
                return None;
            }
            // A quote left open runs to the end of the file, but the
            // file's last line break still ends the record, and the line
            // breaks right before it are blank lines, as they are outside
            // quotes: otherwise a change of the final line endings alone
            // would change the record.
            if quoted && buf.ends_with(b"\n") {
                lines -= 1;
                let mut end = buf.len() - 1;
                while let Some(nl) = trim_cr(buf, end).checked_sub(1) {
                    if buf[nl] != b'\n' {
                        break;
                    }
                    end = nl;
                }
                break (end, buf.len());
            }
            break (buf.len(), buf.len());
        };
        i = j + 1;
        match buf[j] {
            b'"' => quoted = !quoted,
            b',' if !quoted => {
                if field == col {
                    key = Some(field_start..j);
                }
                field += 1;
                field_start = j + 1;
            }
            b'\n' if !quoted => break (j, j + 1),
            b'\n' => lines += 1,
            _ => {}
        }
    };
    let len = trim_cr(buf, end);
    if key.is_none() && field == col {
        key = Some(field_start..len.max(field_start));
    }
    Some(Split {
        len,
        next,
        lines: lines + 1,
        key,
    })
}

/// Field `col` of a whole CSV record.
fn csv_field(rec: &[u8], col: u32) -> Option<Range<usize>> {
    csv_record(rec, col, true).and_then(|s| s.key)
}

/// Whether a CSV field, without its quotes, is `name`.
fn unquoted_eq(field: &[u8], name: &[u8]) -> bool {
    let Some(inner) = field
        .strip_prefix(b"\"")
        .and_then(|f| f.strip_suffix(b"\""))
    else {
        return field == name;
    };
    let mut rest = name;
    let mut i = 0;
    while i < inner.len() {
        let c = inner[i];
        i += if c == b'"' && inner.get(i + 1) == Some(&b'"') {
            2
        } else {
            1
        };
        match rest.split_first() {
            Some((&n, tail)) if n == c => rest = tail,
            _ => return false,
        }
    }
    rest.is_empty()
}

/// Field `col` of a tab-separated `COPY` row.
fn tab_field(rec: &[u8], col: u32) -> Option<Range<usize>> {
    let mut start = 0;
    for _ in 0..col {
        start += memchr(b'\t', &rec[start..])? + 1;
    }
    let end = memchr(b'\t', &rec[start..]).map_or(rec.len(), |e| start + e);
    Some(start..end)
}

/// The table name and the column list of a `COPY name (cols) FROM stdin;`
/// line.
fn copy_header(line: &[u8]) -> Option<(&[u8], &[u8])> {
    let rest = line.strip_prefix(b"COPY ")?.strip_suffix(b" FROM stdin;")?;
    let open = memchr(b'(', rest)?;
    let cols = rest[open + 1..].strip_suffix(b")")?;
    Some((rest[..open].trim_ascii_end(), cols))
}

/// The position of `name` in a `COPY` column list, quoted or not.
fn copy_column(cols: &[u8], name: &[u8]) -> Option<u32> {
    let pos = cols
        .split(|&c| c == b',')
        .position(|c| unquoted_eq(c.trim_ascii(), name))?;
    u32::try_from(pos).ok()
}

/// The value of the field at `path` in a JSON object, raw.
fn json_key(line: &[u8], path: &[Box<str>]) -> Option<Range<usize>> {
    let mut at = skip_ws(line, 0);
    let mut found = None;
    for name in path {
        let r = json_field(line, at, name.as_bytes())?;
        at = r.start;
        found = Some(r);
    }
    found
}

fn skip_ws(s: &[u8], mut i: usize) -> usize {
    while s.get(i).is_some_and(u8::is_ascii_whitespace) {
        i += 1;
    }
    i
}

/// The value of member `name` of the object starting at `s[i]`.
fn json_field(s: &[u8], i: usize, name: &[u8]) -> Option<Range<usize>> {
    if s.get(i) != Some(&b'{') {
        return None;
    }
    let mut i = skip_ws(s, i + 1);
    loop {
        if s.get(i) != Some(&b'"') {
            return None;
        }
        let key_end = string_end(s, i)?;
        let is_name = &s[i + 1..key_end - 1] == name;
        i = skip_ws(s, key_end);
        if s.get(i) != Some(&b':') {
            return None;
        }
        let start = skip_ws(s, i + 1);
        let end = value_end(s, start)?;
        if is_name {
            return Some(start..end);
        }
        i = skip_ws(s, end);
        match s.get(i) {
            Some(b',') => i = skip_ws(s, i + 1),
            _ => return None,
        }
    }
}

/// One past the closing quote of the string starting at `s[i]`.
fn string_end(s: &[u8], mut i: usize) -> Option<usize> {
    i += 1;
    loop {
        i += memchr2(b'"', b'\\', s.get(i..)?)?;
        if s[i] == b'"' {
            return Some(i + 1);
        }
        i += 2;
    }
}

/// One past the JSON value starting at `s[i]`.
fn value_end(s: &[u8], mut i: usize) -> Option<usize> {
    match *s.get(i)? {
        b'"' => string_end(s, i),
        b'{' | b'[' => {
            let mut depth = 0usize;
            loop {
                match *s.get(i)? {
                    b'"' => {
                        i = string_end(s, i)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(i + 1);
                        }
                    }
                    _ => {}
                }
                i += 1;
            }
        }
        _ => {
            let start = i;
            while s
                .get(i)
                .is_some_and(|c| !matches!(c, b',' | b'}' | b']') && !c.is_ascii_whitespace())
            {
                i += 1;
            }
            (i > start).then_some(i)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn csv(buf: &[u8], col: u32) -> Option<(usize, usize, u64, Option<&[u8]>)> {
        csv_record(buf, col, false).map(|s| (s.len, s.next, s.lines, s.key.map(|k| &buf[k])))
    }

    #[test]
    fn csv_records_respect_quotes() {
        assert_eq!(csv(b"1,a,b\n2", 0), Some((5, 6, 1, Some(&b"1"[..]))));
        assert_eq!(csv(b"1,a,b\n", 2), Some((5, 6, 1, Some(&b"b"[..]))));
        assert_eq!(csv(b"1,a,b\r\n", 2), Some((5, 7, 1, Some(&b"b"[..]))));
        assert_eq!(csv(b"1,a\n", 2), Some((3, 4, 1, None)));
        assert_eq!(
            csv(b"\"x,\ny\",2\nz", 0),
            Some((8, 9, 2, Some(&b"\"x,\ny\""[..])))
        );
        assert_eq!(
            csv(b"\"a\"\"b\",c\n", 0),
            Some((8, 9, 1, Some(&b"\"a\"\"b\""[..])))
        );
        assert_eq!(csv(b"\"open,\n", 0), None, "quote still open");
        assert_eq!(csv(b"1,2", 1), None, "no terminator yet");
        let last = csv_record(b"1,2", 1, true).unwrap();
        assert_eq!((last.len, last.next, last.key), (3, 3, Some(2..3)));
        let empty = csv_record(b"\n", 0, false).unwrap();
        assert_eq!((empty.len, empty.next), (0, 1));
    }

    #[test]
    fn open_quote_at_eof_leaves_out_the_last_terminator() {
        for (buf, len) in [
            (&b"2,\"a\nb\n"[..], 6),
            (b"2,\"a\nb\r\n", 6),
            (b"2,\"a\nb", 6),
        ] {
            let s = csv_record(buf, 0, true).unwrap();
            assert_eq!((s.len, s.next, s.lines), (len, buf.len(), 2), "{buf:?}");
        }
    }

    #[test]
    fn open_quote_at_eof_leaves_out_trailing_blank_lines() {
        for (buf, lines) in [
            (&b"2,\"a\n\n"[..], 2),
            (b"2,\"a\r\n\r\n", 2),
            (b"2,\"a\n\r\n\n", 3),
        ] {
            let s = csv_record(buf, 0, true).unwrap();
            assert_eq!((s.len, s.next, s.lines), (4, buf.len(), lines), "{buf:?}");
        }
    }

    #[test]
    fn header_names_compare_without_quotes() {
        assert!(unquoted_eq(b"id", b"id"));
        assert!(unquoted_eq(b"\"id\"", b"id"));
        assert!(unquoted_eq(b"\"a\"\"b\"", b"a\"b"));
        assert!(!unquoted_eq(b"\"idx\"", b"id"));
        assert!(!unquoted_eq(b"\"i\"", b"id"));
    }

    #[test]
    fn json_paths_find_raw_values() {
        let path = |p: &str| p.split('.').map(Into::into).collect::<Vec<Box<str>>>();
        let line = br#"{"a":{"x":[1,{"id":0}],"id":"7\"q"}, "id" : 12 ,"z":"}"}"#;
        let get = |p: &str| json_key(line, &path(p)).map(|r| &line[r]);
        assert_eq!(get("id"), Some(&b"12"[..]));
        assert_eq!(get("a.id"), Some(&br#""7\"q""#[..]));
        assert_eq!(get("z"), Some(&br#""}""#[..]));
        assert_eq!(get("a.x"), Some(&br#"[1,{"id":0}]"#[..]));
        assert_eq!(get("missing"), None);
        assert_eq!(get("id.deeper"), None);
        assert_eq!(json_key(b"[1]", &path("id")), None);
        assert_eq!(json_key(br#"{"id":"#, &path("id")), None);
    }

    #[test]
    fn copy_headers_and_rows() {
        let (table, cols) =
            copy_header(b"COPY public.orders (id, \"Name\", note) FROM stdin;").unwrap();
        assert_eq!(table, b"public.orders");
        assert_eq!(copy_column(cols, b"Name"), Some(1));
        assert_eq!(copy_column(cols, b"note"), Some(2));
        assert_eq!(copy_column(cols, b"other"), None);
        assert_eq!(copy_header(b"COPY t FROM stdin;"), None);
        assert_eq!(copy_header(b"SELECT 1;"), None);
        let row = b"7\tx\t\\N";
        assert_eq!(tab_field(row, 0).map(|r| &row[r]), Some(&b"7"[..]));
        assert_eq!(tab_field(row, 2).map(|r| &row[r]), Some(&b"\\N"[..]));
        assert_eq!(tab_field(row, 3), None);
    }

    fn records(spec: &str, data: &[u8], buf: usize) -> Result<Vec<(u64, String, u32)>> {
        let budget = Budget::new(Bytes::mib(1));
        let spec: KeySpec = spec.parse().unwrap();
        let mut out = Vec::new();
        // One chunk, then many: the chunks must agree with one thread,
        // table ids included, so each starts with no tables named.
        for chunks in [1, 4, 16] {
            let tables = Mutex::new(Tables::new(&budget)?);
            let mut r = Reader::new(data, Side::A, &spec, &budget, buf, chunks)?;
            let filter = Filter::new(0, u64::MAX, &[]);
            let mut runs = Vec::new();
            for _ in 0..2 {
                let mut got = Vec::new();
                r.rewind();
                while r.advance(&tables, &filter)? {
                    for rec in r.kept() {
                        let key = String::from_utf8_lossy(&rec.bytes[rec.key.clone()]).into_owned();
                        got.push((rec.line, key, rec.table));
                    }
                }
                assert_eq!(r.consumed(), data.len() as u64);
                assert_eq!(r.records(), got.len() as u64);
                runs.push(got);
            }
            assert_eq!(runs[0], runs[1]);
            if chunks > 1 {
                assert_eq!(runs[0], out, "{chunks} chunks");
            }
            out = runs.swap_remove(0);
        }
        Ok(out)
    }

    #[test]
    fn chunks_that_guess_wrong_are_parsed_again() {
        use std::fmt::Write;

        // Quoted line breaks land on chunk cuts, and COPY headers and
        // terminators change the state between chunks.
        let mut csv = String::from("id,v\n");
        for i in 0..300 {
            if i % 7 == 0 {
                write!(csv, "{i},\"a\nb\nc,\"\"q\"\"\n\"\n").unwrap();
            } else {
                writeln!(csv, "{i},x{i}").unwrap();
            }
        }
        let got = records("csv:id", csv.as_bytes(), 1 << 10).unwrap();
        assert_eq!(got.len(), 301);
        assert!(
            got.iter()
                .skip(1)
                .enumerate()
                .all(|(i, r)| r.1 == i.to_string())
        );
        // Rows of a one-column table that read like COPY headers.
        let mut dump = String::new();
        for t in 0..20 {
            writeln!(dump, "COPY t{t} (id) FROM stdin;").unwrap();
            for i in 0..t * 3 {
                writeln!(dump, "COPY x{i} (id) FROM stdin;").unwrap();
            }
            dump.push_str("\\.\nSELECT 1;\n");
        }
        let got = records("copy:id", dump.as_bytes(), 256).unwrap();
        assert_eq!(got.len(), (0..20).map(|t| t * 3).sum::<usize>());
        let mut tables: Vec<u32> = got.iter().map(|r| r.2).collect();
        tables.dedup();
        assert_eq!(tables.len(), 19, "{tables:?}");
    }

    #[test]
    fn readers_number_records_by_their_first_line() {
        let data = b"id,v\n1,\"two\nlines\"\n\n2,x\r\n3,y";
        let want = |k: &[&str]| -> Vec<(u64, String, u32)> {
            [0, 1, 4, 5]
                .iter()
                .zip(k)
                .map(|(&l, k)| (l, (*k).into(), 0))
                .collect()
        };
        for buf in [4, 64] {
            assert_eq!(
                records("csv:1", data, buf).unwrap(),
                want(&["id", "1", "2", "3"])
            );
            assert_eq!(
                records("csv:v", data, buf).unwrap(),
                want(&["v", "\"two\nlines\"", "x", "y"])
            );
        }
        assert!(matches!(
            records("csv:w", data, 64),
            Err(Error::Keyed(KeyError::NoColumn { side: Side::A, .. }))
        ));
        assert!(matches!(
            records("csv:3", data, 64),
            Err(Error::Keyed(KeyError::NoKey {
                line: LineNo(0),
                ..
            }))
        ));
    }

    #[test]
    fn copy_rows_outside_keyed_blocks_are_skipped() {
        let dump = b"SET x;\nCOPY a (id, v) FROM stdin;\n1\tx\n\\.\nCOPY b (v) FROM stdin;\n9\n\\.\nCOPY c (v, id) FROM stdin;\ny\t1\n\\.\n";
        let got = records("copy:id", dump, 16).unwrap();
        assert_eq!(got, [(2, "1".into(), 1), (8, "1".into(), 2)]);
    }

    #[test]
    fn ndjson_without_the_field_is_an_error() {
        let data = b"{\"id\":1}\n{\"x\":2}\n";
        assert!(matches!(
            records("json:id", data, 64),
            Err(Error::Keyed(KeyError::NoKey {
                line: LineNo(1),
                ..
            }))
        ));
    }
}
