//! Which fields of a changed record differ: CSV columns, NDJSON top-level
//! members, `COPY` columns.
//!
//! [`compare_fields`] runs the passes of [`compare_with_ranges`] and, for
//! each change, reads its records back into one buffer held in the refine
//! half, splits them into fields and compares the fields by their bytes.
//! The records were in memory during the pass, but the pass hands its
//! changes out sorted by line once it ends, so a field diff found while
//! the records met would have to be kept per change; reading them back
//! keeps one pair in memory at a time.
//!
//! [`compare_with_ranges`]: super::compare_with_ranges

use std::io;
use std::ops::Range;

use super::pass::{self, Fielded, MAX_PASSES, Report, TabledChange};
use super::read::{Tables, skip_ws, string_end, unquoted_eq, value_end};
use super::{ChangeKind, CsvColumn, KeyError, KeySpec, RangedChange, Side};
use crate::budget::{Budget, BudgetExceeded, Half, Reservation};
use crate::error::{Error, Result};
use crate::source::Source;
use crate::types::{ByteOffset, Bytes};

/// Record bytes the buffer holds before it grows, from the refine half.
const START: usize = 4 << 10;
/// Fields per record, and differing fields, the lists hold before they
/// grow. Reserved before the first pass: at emit the pass holds most of
/// the refine half, and a small pair should not need what is left.
const START_FIELDS: usize = 64;

/// No field on this side.
const NONE: u32 = u32::MAX;

/// How a record splits into fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    /// CSV keyed by column number: fields are numbered from 1.
    CsvNumbered,
    /// CSV keyed by a header name: fields are named by each file's header.
    CsvNamed,
    /// NDJSON: top-level members.
    Ndjson,
    /// `COPY` rows: named by the column list of the record's table.
    Copy,
}

impl Format {
    fn of(spec: &KeySpec) -> Self {
        match spec {
            KeySpec::Csv(CsvColumn::Index(_)) => Self::CsvNumbered,
            KeySpec::Csv(CsvColumn::Header(_)) => Self::CsvNamed,
            KeySpec::Ndjson(_) => Self::Ndjson,
            KeySpec::Copy(_) => Self::Copy,
        }
    }

    fn csv(self) -> bool {
        matches!(self, Self::CsvNumbered | Self::CsvNamed)
    }
}

/// A field of a record in the buffer, its offsets relative to the record.
#[derive(Debug, Clone)]
struct Field {
    name: Name,
    value: Range<u32>,
}

const _: () = assert!(size_of::<Field>() == 20);

#[derive(Debug, Clone)]
enum Name {
    Number(u32),
    /// In the record: an NDJSON member name.
    Record(Range<u32>),
    /// In the side's header.
    Head(Range<u32>),
}

/// What a field is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldName<'c> {
    /// A CSV column number, from 1, for `csv:N` keys or for cells past
    /// the header.
    Number(u32),
    /// A header cell, `COPY` column name or NDJSON member name, as the
    /// file has it.
    Bytes(&'c [u8]),
}

/// One field that differs between the two records of a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldDiff<'c> {
    /// The field, as A names it, or as B does when A lacks it.
    pub name: FieldName<'c>,
    /// Its bytes in A as the file has them, `None` when A's record lacks it.
    pub a: Option<&'c [u8]>,
    /// Its bytes in B, `None` when B's record lacks it.
    pub b: Option<&'c [u8]>,
}

/// A change from [`compare_fields`], with its records and the fields that
/// differ.
#[derive(Debug)]
pub struct FieldChange<'c> {
    /// The key, its kind, its lines and its ranges.
    pub change: &'c RangedChange,
    /// A's record, without its line terminator.
    pub a_record: Option<&'c [u8]>,
    /// B's record, without its line terminator.
    pub b_record: Option<&'c [u8]>,
    records: [&'c [u8]; 2],
    heads: [Option<&'c [u8]>; 2],
    fields: [&'c [Field]; 2],
    diffs: Option<&'c [(u32, u32)]>,
}

impl<'c> FieldChange<'c> {
    /// The fields that differ, in A's order, then those only B has in B's
    /// order. Empty for an added or removed key, and for a changed key
    /// whose records differ only in layout. `None` when a record does not
    /// split into fields: an NDJSON line that is not one JSON object.
    #[must_use]
    pub fn fields(&self) -> Option<impl Iterator<Item = FieldDiff<'c>> + 'c> {
        let (records, heads, fields) = (self.records, self.heads, self.fields);
        let field = move |side: usize, i: u32| -> Option<(FieldName<'c>, &'c [u8])> {
            let f = fields[side].get(i as usize)?;
            let rec = records[side];
            Some((field_name(f, rec, heads[side]), &rec[span(&f.value)]))
        };
        let diffs = self.diffs?;
        Some(diffs.iter().filter_map(move |&(i, j)| {
            let (a, b) = (field(0, i), field(1, j));
            Some(FieldDiff {
                name: a.or(b)?.0,
                a: a.map(|f| f.1),
                b: b.map(|f| f.1),
            })
        }))
    }
}

fn field_name<'r>(f: &Field, rec: &'r [u8], head: Option<&'r [u8]>) -> FieldName<'r> {
    match &f.name {
        Name::Number(n) => FieldName::Number(*n),
        Name::Record(r) => FieldName::Bytes(&rec[span(r)]),
        Name::Head(r) => FieldName::Bytes(&head.unwrap_or_default()[span(r)]),
    }
}

fn span(r: &Range<u32>) -> Range<usize> {
    r.start as usize..r.end as usize
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "a record lies in a read window, which stays below 4 GiB"
)]
fn at(r: Range<usize>) -> Range<u32> {
    r.start as u32..r.end as u32
}

/// [`compare_with_ranges`](super::compare_with_ranges), handing out each
/// change with its records and the fields that differ: CSV columns,
/// NDJSON top-level members, `COPY` columns.
///
/// With `select`, only those fields count: a changed key whose selected
/// fields are equal is not handed out and not counted in
/// [`Report::changes`]. Added and removed keys always are.
///
/// Fields are named by each file's own header (CSV keyed by a header name,
/// `COPY`) and matched by name, so a column that moved is not a change.
/// CSV keyed by number names its fields by number. Two fields are equal
/// when their bytes are: CSV cells after removing their quotes, with an
/// unquoted empty cell (NULL in a Postgres CSV export) apart from `""`;
/// NDJSON values and `COPY` cells as they are, so `\N` differs from an
/// empty cell. A nested NDJSON value is one field.
///
/// Each change's records are read back into one buffer from the refine
/// half, 4 KiB and room for 64 fields at first, reserved before the first
/// pass, grown to the largest pair and kept to the end.
///
/// # Errors
///
/// As [`compare`](super::compare), and also a budget error when a pair of
/// records does not fit the refine half, and [`KeyError::NoColumn`] when a
/// selected field is in no header, or is not a column number for a
/// `csv:N` key.
pub fn compare_fields<A, B>(
    a: &A,
    b: &B,
    spec: &KeySpec,
    select: Option<&[String]>,
    budget: &Budget,
    mut on_change: impl FnMut(&FieldChange<'_>) -> io::Result<()>,
) -> Result<Report>
where
    A: Source + ?Sized,
    B: Source + ?Sized,
{
    let format = Format::of(spec);
    if format == Format::CsvNumbered {
        for name in select.unwrap_or_default() {
            if !matches!(name.parse::<u32>(), Ok(n) if n > 0) {
                return Err(no_column(name));
            }
        }
    }
    let mut pair = Pair::new(budget)?;
    let (mut failed, mut skipped) = (None, 0u64);
    let report = pass::run::<A, B, Fielded>(a, b, spec, budget, MAX_PASSES, |c, tables| {
        match pair.load(a, b, c, tables, format, select) {
            Ok(Some(change)) => on_change(&change),
            Ok(None) => {
                skipped += 1;
                Ok(())
            }
            Err(Error::Io(e)) => Err(e),
            // The pass takes an I/O error from its callback; the real one
            // replaces it below.
            Err(e) => {
                let err = io::Error::other(e.to_string());
                failed = Some(e);
                Err(err)
            }
        }
    });
    // Before the refusal is built, so its numbers do not count the buffer.
    drop(pair);
    let mut report = report
        .map_err(|e| failed.take().unwrap_or(e))
        .map_err(|e| pass::doubled(e, budget))?;
    report.changes -= skipped;
    Ok(report)
}

fn no_column(name: &str) -> Error {
    KeyError::NoColumn {
        side: Side::A,
        name: name.into(),
    }
    .into()
}

/// The records of one change and their fields, in buffers held in the
/// refine half.
#[derive(Debug)]
struct Pair {
    bytes: Vec<u8>,
    fields: [Vec<Field>; 2],
    diffs: Vec<(u32, u32)>,
    res: Reservation,
}

impl Pair {
    fn new(budget: &Budget) -> Result<Self> {
        let bytes = START + START_FIELDS * (2 * size_of::<Field>() + size_of::<(u32, u32)>());
        let res = budget.try_reserve(Half::Refine, Bytes(bytes as u64))?;
        Ok(Self {
            bytes: Vec::with_capacity(START),
            fields: [(); 2].map(|()| Vec::with_capacity(START_FIELDS)),
            diffs: Vec::with_capacity(START_FIELDS),
            res,
        })
    }

    /// Reads the records of `c` and finds the fields that differ; `None`
    /// when `select` leaves no field that differs.
    fn load<'p, A, B>(
        &'p mut self,
        a: &A,
        b: &B,
        c: &'p TabledChange,
        tables: &'p Tables,
        format: Format,
        select: Option<&[String]>,
    ) -> Result<Option<FieldChange<'p>>>
    where
        A: Source + ?Sized,
        B: Source + ?Sized,
    {
        let ranged = &c.ranged;
        let len = |r: &Option<Range<u64>>| {
            r.as_ref().map_or(0, |r| {
                usize::try_from(r.end - r.start).unwrap_or(usize::MAX)
            })
        };
        let (la, lb) = (len(&ranged.a_range), len(&ranged.b_range));
        self.bytes.clear();
        room(&mut self.bytes, &mut self.res, la.saturating_add(lb))?;
        self.bytes.resize(la + lb, 0);
        let (ba, bb) = self.bytes.split_at_mut(la);
        if let Some(r) = &ranged.a_range {
            a.read_exact_at(ba, ByteOffset(r.start))
                .map_err(Error::Io)?;
        }
        if let Some(r) = &ranged.b_range {
            b.read_exact_at(bb, ByteOffset(r.start))
                .map_err(Error::Io)?;
        }
        let records = [&self.bytes[..la], &self.bytes[la..]];
        let table = match format {
            Format::CsvNamed => Some(0),
            Format::Copy => Some(c.table),
            Format::CsvNumbered | Format::Ndjson => None,
        };
        let heads = [Side::A, Side::B].map(|s| table.and_then(|t| tables.head(t, s)));
        if let (Some(select), Some(_)) = (select, table) {
            for name in select {
                let named =
                    |h: &[u8]| head_names(format, h).any(|r| unquoted_eq(&h[r], name.as_bytes()));
                if !tables.heads().any(named) {
                    return Err(no_column(name));
                }
            }
        }
        let [fa, fb] = &mut self.fields;
        fa.clear();
        fb.clear();
        self.diffs.clear();
        let parsed = ranged.change.kind == ChangeKind::Changed
            && split(format, records[0], heads[0], fa, &mut self.res)?
            && split(format, records[1], heads[1], fb, &mut self.res)?;
        if parsed {
            let fields = [&fa[..], &fb[..]];
            diff(
                format,
                records,
                heads,
                fields,
                select,
                &mut self.diffs,
                &mut self.res,
            )?;
            if select.is_some() && self.diffs.is_empty() {
                return Ok(None);
            }
        }
        let changed = ranged.change.kind == ChangeKind::Changed;
        Ok(Some(FieldChange {
            change: ranged,
            a_record: ranged.a_range.as_ref().map(|_| records[0]),
            b_record: ranged.b_range.as_ref().map(|_| records[1]),
            records,
            heads,
            fields: [&self.fields[0], &self.fields[1]],
            diffs: (parsed || !changed).then_some(&self.diffs[..]),
        }))
    }
}

/// Grows `v` to hold `need` more, from `res`, doubling.
fn room<T>(v: &mut Vec<T>, res: &mut Reservation, need: usize) -> Result<(), BudgetExceeded> {
    let free = v.capacity() - v.len();
    if free >= need {
        return Ok(());
    }
    let add = (need - free).max(v.capacity());
    res.grow(Bytes((add as u64).saturating_mul(size_of::<T>() as u64)))?;
    v.reserve_exact(v.capacity() + add - v.len());
    Ok(())
}

fn push(v: &mut Vec<Field>, res: &mut Reservation, f: Field) -> Result<(), BudgetExceeded> {
    room(v, res, 1)?;
    v.push(f);
    Ok(())
}

/// Splits `rec` into `out`; false when it does not split (an NDJSON line
/// that is not one object).
fn split(
    format: Format,
    rec: &[u8],
    head: Option<&[u8]>,
    out: &mut Vec<Field>,
    res: &mut Reservation,
) -> Result<bool, BudgetExceeded> {
    let number = |i: usize| Name::Number(u32::try_from(i + 1).unwrap_or(u32::MAX));
    if format == Format::Ndjson {
        return members(rec, |name, value| {
            push(
                out,
                res,
                Field {
                    name: Name::Record(at(name)),
                    value: at(value),
                },
            )
        });
    }
    for (i, r) in row_cells(format, rec).enumerate() {
        push(
            out,
            res,
            Field {
                name: number(i),
                value: at(r),
            },
        )?;
    }
    if let Some(head) = head {
        for (f, r) in out.iter_mut().zip(head_names(format, head)) {
            f.name = Name::Head(at(r));
        }
    }
    Ok(true)
}

/// The cells of a CSV record or a `COPY` row.
fn row_cells(format: Format, rec: &[u8]) -> impl Iterator<Item = Range<usize>> + '_ {
    if format.csv() {
        cells(rec, b',', true)
    } else {
        cells(rec, b'\t', false)
    }
}

/// The names in a CSV header row or a `COPY` column list, the latter
/// without the spaces around them.
fn head_names(format: Format, head: &[u8]) -> impl Iterator<Item = Range<usize>> + '_ {
    let copy = format == Format::Copy;
    cells(head, b',', !copy).map(move |r| {
        if !copy {
            return r;
        }
        let cell = &head[r.clone()];
        let lead = cell.len() - cell.trim_ascii_start().len();
        let trail = cell.len() - cell.trim_ascii_end().len();
        r.start + lead..(r.end - trail).max(r.start + lead)
    })
}

/// The cells of `rec` between `sep` bytes; with `quoting`, a `sep` inside
/// quotes is data, as the reader splits CSV records.
fn cells(rec: &[u8], sep: u8, quoting: bool) -> impl Iterator<Item = Range<usize>> + '_ {
    let (mut start, mut i, mut quoted, mut done) = (0, 0, false, false);
    std::iter::from_fn(move || {
        while i < rec.len() {
            let c = rec[i];
            i += 1;
            if quoting && c == b'"' {
                quoted = !quoted;
            } else if c == sep && !quoted {
                let r = start..i - 1;
                start = i;
                return Some(r);
            }
        }
        if done {
            return None;
        }
        done = true;
        Some(start..rec.len())
    })
}

/// Calls `each` with the name (inside its quotes) and the raw value of
/// every top-level member of the JSON object `s`; false when `s` is not
/// one object.
fn members<E>(
    s: &[u8],
    mut each: impl FnMut(Range<usize>, Range<usize>) -> Result<(), E>,
) -> Result<bool, E> {
    let mut i = skip_ws(s, 0);
    if s.get(i) != Some(&b'{') {
        return Ok(false);
    }
    i = skip_ws(s, i + 1);
    if s.get(i) == Some(&b'}') {
        return Ok(skip_ws(s, i + 1) == s.len());
    }
    loop {
        if s.get(i) != Some(&b'"') {
            return Ok(false);
        }
        let Some(name_end) = string_end(s, i) else {
            return Ok(false);
        };
        let name = i + 1..name_end - 1;
        i = skip_ws(s, name_end);
        if s.get(i) != Some(&b':') {
            return Ok(false);
        }
        let start = skip_ws(s, i + 1);
        let Some(end) = value_end(s, start) else {
            return Ok(false);
        };
        each(name, start..end)?;
        i = skip_ws(s, end);
        match s.get(i) {
            Some(b',') => i = skip_ws(s, i + 1),
            Some(b'}') => return Ok(skip_ws(s, i + 1) == s.len()),
            _ => return Ok(false),
        }
    }
}

/// Pushes to `out` the index pairs of the fields that differ, `NONE` for
/// a side that lacks the field.
// Matching by name is O(fields²) per changed record; a sorted
// index if records with thousands of fields show up in a profile.
fn diff(
    format: Format,
    records: [&[u8]; 2],
    heads: [Option<&[u8]>; 2],
    fields: [&[Field]; 2],
    select: Option<&[String]>,
    out: &mut Vec<(u32, u32)>,
    res: &mut Reservation,
) -> Result<(), BudgetExceeded> {
    let index = |i: usize| u32::try_from(i).unwrap_or(NONE);
    let [fa, fb] = fields;
    let name = |side: usize, f: &Field| field_name(f, records[side], heads[side]);
    for (i, f) in fa.iter().enumerate() {
        let n = name(0, f);
        if !selected(format, select, n) {
            continue;
        }
        let j = fb.iter().position(|g| same(format, n, name(1, g)));
        let differs = j.is_none_or(|j| {
            let (x, y) = (&records[0][span(&f.value)], &records[1][span(&fb[j].value)]);
            if format.csv() { !csv_eq(x, y) } else { x != y }
        });
        if differs {
            room(out, res, 1)?;
            out.push((index(i), j.map_or(NONE, index)));
        }
    }
    for (j, g) in fb.iter().enumerate() {
        let n = name(1, g);
        if selected(format, select, n) && !fa.iter().any(|f| same(format, name(0, f), n)) {
            room(out, res, 1)?;
            out.push((NONE, index(j)));
        }
    }
    Ok(())
}

/// Whether `select` lets field `n` count.
fn selected(format: Format, select: Option<&[String]>, n: FieldName<'_>) -> bool {
    select.is_none_or(|list| {
        list.iter().any(|s| match n {
            FieldName::Number(i) => s.parse() == Ok(i),
            FieldName::Bytes(b) if format == Format::Ndjson => b == s.as_bytes(),
            FieldName::Bytes(b) => unquoted_eq(b, s.as_bytes()),
        })
    })
}

/// Whether A's field `x` and B's field `y` are the same field.
fn same(format: Format, x: FieldName<'_>, y: FieldName<'_>) -> bool {
    match (x, y) {
        (FieldName::Number(x), FieldName::Number(y)) => x == y,
        (FieldName::Bytes(x), FieldName::Bytes(y)) if format == Format::Ndjson => x == y,
        (FieldName::Bytes(x), FieldName::Bytes(y)) => x == y || unquote(x).eq(unquote(y)),
        _ => false,
    }
}

/// Whether two CSV cells hold the same value once unquoted. An unquoted
/// empty cell is NULL in a Postgres CSV export and `""` an empty string,
/// so an empty cell equals only another empty cell.
fn csv_eq(a: &[u8], b: &[u8]) -> bool {
    if a.is_empty() || b.is_empty() {
        return a == b;
    }
    a == b || unquote(a).eq(unquote(b))
}

/// A CSV cell without its quotes, `""` read as `"`.
fn unquote(cell: &[u8]) -> impl Iterator<Item = u8> + Clone + '_ {
    unquote_at(cell).map(|(_, c)| c)
}

/// [`unquote`] with each byte's offset in `cell`.
fn unquote_at(cell: &[u8]) -> impl Iterator<Item = (usize, u8)> + Clone + '_ {
    let (inner, quoted) = match cell {
        [b'"', inner @ .., b'"'] => (inner, true),
        _ => (cell, false),
    };
    let mut second = false;
    inner
        .iter()
        .enumerate()
        .map(move |(i, &c)| (i + usize::from(quoted), c))
        .filter(move |&(_, c)| {
            if quoted && c == b'"' {
                second = !second;
                second
            } else {
                second = false;
                true
            }
        })
}

/// Where two CSV cells first differ once unquoted, as an offset in each
/// cell's own bytes. A cell that ends first, being the other's prefix,
/// reports its length. Allocates nothing.
#[must_use]
pub fn csv_first_difference(a: &[u8], b: &[u8]) -> (usize, usize) {
    let (mut left, mut right) = (unquote_at(a), unquote_at(b));
    loop {
        match (left.next(), right.next()) {
            (Some((_, x)), Some((_, y))) if x == y => {}
            (Some((at_a, _)), Some((at_b, _))) => return (at_a, at_b),
            (Some((at_a, _)), None) => return (at_a, b.len()),
            (None, Some((at_b, _))) => return (a.len(), at_b),
            (None, None) => return (a.len(), b.len()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use proptest::prelude::*;

    use super::*;

    /// Each change as `KEY: name a -> b; ...`, values `(none)` when absent,
    /// `?` when the records did not split; sorted.
    fn changes(
        a: &[u8],
        b: &[u8],
        spec: &str,
        select: Option<&[&str]>,
        mem: u64,
    ) -> Result<(Vec<String>, Report)> {
        let spec: KeySpec = spec.parse().unwrap();
        let select: Option<Vec<String>> = select.map(|s| s.iter().map(|&n| n.into()).collect());
        let text = |b: &[u8]| String::from_utf8_lossy(b).into_owned();
        let mut out = Vec::new();
        let report = compare_fields(
            a,
            b,
            &spec,
            select.as_deref(),
            &Budget::new(Bytes(mem)),
            |c| {
                let mut line =
                    format!("{:?} {}:", c.change.change.kind, text(&c.change.change.key));
                match c.fields() {
                    None => line.push_str(" ?"),
                    Some(fields) => {
                        for f in fields {
                            let name = match f.name {
                                FieldName::Number(n) => n.to_string(),
                                FieldName::Bytes(b) => text(b),
                            };
                            let v = |v: Option<&[u8]>| v.map_or_else(|| "(none)".into(), text);
                            write!(line, " {name} {} -> {};", v(f.a), v(f.b)).unwrap();
                        }
                    }
                }
                out.push(line);
                Ok(())
            },
        )?;
        out.sort_unstable();
        Ok((out, report))
    }

    fn fields(a: &[u8], b: &[u8], spec: &str) -> Vec<String> {
        changes(a, b, spec, None, 1 << 20).unwrap().0
    }

    #[test]
    fn csv_fields_are_named_by_the_header_and_keep_their_quotes() {
        let a =
            b"id,name,price,note\n1,apple,10,\"red, sweet\"\n3,plum,5,\"two\nlines\"\n4,fig,3,x\n";
        let b = b"id,name,price,note\n3,plum,5,\"two\nLINES\"\n1,apple,12,\"red, sour\"\n5,kiwi,4,new\n";
        assert_eq!(
            fields(a, b, "csv:id"),
            [
                "Added 5:",
                "Changed 1: price 10 -> 12; note \"red, sweet\" -> \"red, sour\";",
                "Changed 3: note \"two\nlines\" -> \"two\nLINES\";",
                "Removed 4:",
            ]
        );
    }

    #[test]
    fn csv_difference_is_found_in_each_cells_own_bytes() {
        assert_eq!(csv_first_difference(b"\"ab\"\"c\"", b"ab\"d"), (5, 3));
        assert_eq!(csv_first_difference(b"abc", b"ab"), (2, 2));
        assert_eq!(csv_first_difference(b"\"ab\"", b"ab"), (4, 2));
    }

    #[test]
    fn csv_columns_match_by_name_and_compare_unquoted() {
        // Columns swapped, and a cell quoted in one file only: no field
        // differs, though the records do.
        let a = b"id,a,b\n1,x,\"y\"\n";
        let b = b"id,b,a\n1,y,x\n";
        // The header row is a record too, keyed by its own cell.
        assert_eq!(fields(a, b, "csv:id"), ["Changed 1:", "Changed id:"]);
        // An unquoted empty cell is NULL, `""` the empty string.
        let a = b"id,a\n1,\n2,\"say \"\"hi\"\"\"\n";
        let b = b"id,a\n1,\"\"\n2,say \"hi\"\n";
        assert_eq!(
            fields(a, b, "csv:id"),
            ["Changed 1: a  -> \"\";", "Changed 2:"]
        );
    }

    #[test]
    fn csv_keyed_by_number_numbers_its_fields() {
        let a = b"1,x,y\n2,p\n";
        let b = b"1,x,z\n2,p,q\n";
        assert_eq!(
            fields(a, b, "csv:1"),
            ["Changed 1: 3 y -> z;", "Changed 2: 3 (none) -> q;"]
        );
    }

    #[test]
    fn ndjson_fields_are_top_level_members_with_raw_values() {
        let a = b"{\"id\":1,\"price\":10,\"tags\":[\"a\",\"b\"],\"user\":{\"n\":\"x\"}}\n{\"id\":2,\"price\":7,\"old\":true}\n{\"id\":3, \"v\": 1}\n{\"id\":4,\"v\":tru e}\n";
        let b = b"{\"id\":1,\"price\":12,\"tags\":[\"a\",\"c\"],\"user\":{\"n\": \"x\"}}\n{\"price\":7,\"id\":2,\"new\":\"y\"}\n{\"v\":1,\"id\":3}\n{\"id\":4,\"v\":true}\n";
        assert_eq!(
            fields(a, b, "json:id"),
            [
                "Changed 1: price 10 -> 12; tags [\"a\",\"b\"] -> [\"a\",\"c\"]; user {\"n\":\"x\"} -> {\"n\": \"x\"};",
                "Changed 2: old true -> (none); new (none) -> \"y\";",
                "Changed 3:",
                "Changed 4: ?",
            ]
        );
    }

    #[test]
    fn copy_fields_are_named_by_their_tables_columns() {
        let a = b"COPY public.item (id, name, price) FROM stdin;\n1\tapple\t10\n2\tpear\t\\N\n\\.\nCOPY public.stock (id, qty) FROM stdin;\n1\t5\n\\.\n";
        let b = b"COPY public.item (id, price, name) FROM stdin;\n2\t\tpear\n1\t12\tapple\n\\.\nCOPY public.stock (id, qty) FROM stdin;\n1\t6\n\\.\n";
        assert_eq!(
            fields(a, b, "copy:id"),
            [
                "Changed 1: price 10 -> 12;",
                "Changed 1: qty 5 -> 6;",
                "Changed 2: price \\N -> ;"
            ]
        );
    }

    #[test]
    fn selected_fields_decide_what_counts_as_changed() {
        let a = b"id,price,note\n1,10,a\n2,7,b\n3,1,c\n";
        let b = b"id,price,note\n1,10,A\n2,8,b\n4,1,c\n";
        let (out, report) = changes(a, b, "csv:id", Some(&["price"]), 1 << 20).unwrap();
        assert_eq!(out, ["Added 4:", "Changed 2: price 7 -> 8;", "Removed 3:"]);
        assert_eq!(report.changes, 3);
        let (out, report) = changes(a, b, "csv:id", Some(&["note"]), 1 << 20).unwrap();
        assert_eq!(out, ["Added 4:", "Changed 1: note a -> A;", "Removed 3:"]);
        assert_eq!(report.changes, 3);
        let (out, _) = changes(b"1,x,y\n", b"1,x,z\n", "csv:1", Some(&["2"]), 1 << 20).unwrap();
        assert!(out.is_empty(), "{out:?}");
    }

    #[test]
    fn a_selected_field_in_no_header_is_an_error() {
        let a = b"id,price\n1,10\n";
        let b = b"id,price\n1,11\n";
        let err = changes(a, b, "csv:id", Some(&["prise"]), 1 << 20).unwrap_err();
        assert!(
            matches!(err, Error::Keyed(KeyError::NoColumn { ref name, .. }) if name == "prise"),
            "{err}"
        );
        let err = changes(a, b, "csv:1", Some(&["price"]), 1 << 20).unwrap_err();
        assert!(
            matches!(err, Error::Keyed(KeyError::NoColumn { .. })),
            "{err}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore = "too slow under Miri")]
    fn a_pair_larger_than_the_refine_half_is_a_budget_error() {
        let long = "x".repeat(300 << 10);
        let a = format!("id,v\n1,{long}\n");
        let b = format!("id,v\n1,{long}y\n");
        let mem = 2 << 20;
        let err = changes(a.as_bytes(), b.as_bytes(), "csv:id", None, mem).unwrap_err();
        let Error::Budget(refusal) = err else {
            panic!("{err}");
        };
        assert_eq!(refusal.half, Half::Refine);
        let (out, _) = changes(a.as_bytes(), b.as_bytes(), "csv:id", None, 16 << 20).unwrap();
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn a_pair_that_does_not_fit_is_refused_until_it_does() {
        let budget = Budget::new(Bytes(1 << 20));
        let tables = Tables::with_heads(&budget).unwrap();
        let mut pair = Pair::new(&budget).unwrap();
        let a = vec![b'x'; 100 << 10];
        let len = a.len() as u64;
        let c = TabledChange {
            ranged: RangedChange {
                change: super::super::KeyedChange {
                    key: b"x".to_vec(),
                    kind: ChangeKind::Changed,
                    a: Some(crate::types::LineNo(0)),
                    b: Some(crate::types::LineNo(0)),
                    table: None,
                },
                a_range: Some(0..len),
                b_range: Some(0..len),
            },
            table: 0,
        };
        let free = budget.available(Half::Refine).0;
        let hog = budget
            .try_reserve(Half::Refine, Bytes(free - (100 << 10)))
            .unwrap();
        let err = pair
            .load(&a[..], &a[..], &c, &tables, Format::CsvNumbered, None)
            .unwrap_err();
        assert!(matches!(err, Error::Budget(_)), "{err}");
        drop(hog);
        let change = pair
            .load(&a[..], &a[..], &c, &tables, Format::CsvNumbered, None)
            .unwrap()
            .unwrap();
        assert_eq!(change.a_record, Some(&a[..]));
    }

    /// Rows of `n` keys with three fields each, B changing some fields,
    /// dropping some keys and adding others, in another order.
    type Rows = Vec<(u32, [u32; 3])>;

    fn rows(n: u32, seed: u64) -> (Rows, Rows) {
        let mut x = seed | 1;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        let a: Vec<_> = (0..n).map(|k| (k, [k, k * 2, k * 3])).collect();
        let mut b = Vec::new();
        for &(k, mut v) in &a {
            match next() % 8 {
                0 => continue,
                1 => v[(next() % 3) as usize] += 1,
                2 => v = [v[0] + 1, v[1], v[2] + 1],
                _ => {}
            }
            b.push((k, v));
        }
        b.push((n, [0; 3]));
        let turn = usize::try_from(next() % b.len() as u64).unwrap();
        b.rotate_left(turn);
        (a, b)
    }

    /// The property below, on the input that found a refusal at a budget
    /// the plain passes hold: the pair's lists must not need the refine
    /// half that is left at emit.
    #[test]
    fn a_tight_budget_holds_the_pair_of_a_short_record() {
        let (a, b) = rows(594, 6_640_308_988_055_776_412);
        let csv = |rows: &[(u32, [u32; 3])]| {
            let mut s = String::from("id,p,q,r\n");
            for (k, v) in rows {
                writeln!(s, "{k},{},{},{}", v[0], v[1], v[2]).unwrap();
            }
            s.into_bytes()
        };
        let (fa, fb) = (csv(&a), csv(&b));
        let plain = super::super::compare(
            &fa[..],
            &fb[..],
            &"csv:id".parse().unwrap(),
            &Budget::new(Bytes(167 << 10)),
            |_| Ok(()),
        )
        .unwrap();
        let (_, report) = changes(&fa, &fb, "csv:id", None, 167 << 10).unwrap();
        assert_eq!(report.changes, plain.changes);
    }

    proptest! {
        #[test]
        #[cfg_attr(miri, ignore = "too slow under Miri")]
        fn reported_fields_are_the_fields_that_changed_at_any_budget(n in 1u32..800, seed in any::<u64>(), mem in 64u64..512) {
            let (a, b) = rows(n, seed);
            let csv = |rows: &[(u32, [u32; 3])]| {
                let mut s = String::from("id,p,q,r\n");
                for (k, v) in rows {
                    writeln!(s, "{k},{},{},{}", v[0], v[1], v[2]).unwrap();
                }
                s.into_bytes()
            };
            let mut want = Vec::new();
            for (k, va) in &a {
                match b.iter().find(|r| r.0 == *k) {
                    None => want.push(format!("Removed {k}:")),
                    Some((_, vb)) if va != vb => {
                        let mut line = format!("Changed {k}:");
                        for (i, name) in ["p", "q", "r"].iter().enumerate() {
                            if va[i] != vb[i] {
                                write!(line, " {name} {} -> {};", va[i], vb[i]).unwrap();
                            }
                        }
                        want.push(line);
                    }
                    Some(_) => {}
                }
            }
            want.push(format!("Added {n}:"));
            want.sort_unstable();
            let (got, report) = changes(&csv(&a), &csv(&b), "csv:id", None, mem << 10).unwrap();
            prop_assert_eq!(report.changes, want.len() as u64);
            prop_assert_eq!(got, want);
        }
    }
}
