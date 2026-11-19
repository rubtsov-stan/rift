//! `--key`: records compared by a key column, independent of row order.

use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use rift_core::keyed::{
    ChangeKind, FieldChange, FieldName, KeyError, KeySpec, KeyedChange, RangedChange, RecordReader,
    Side, compare, compare_fields, compare_with_ranges,
};
use rift_core::{Bytes, Error, LineNo, Source};

use crate::source::FileSource;
use crate::summary::peak_heap;

/// Read-back buffer for `--show-records`, from the refine half: a record
/// longer than this streams through it.
const RECORD_BUF: usize = 64 << 10;

/// Bytes of a field value printed before it is cut.
const VALUE_CUT: usize = 64;

/// What key mode prints under each change line.
#[derive(Debug, Clone, Copy)]
pub struct Show<'a> {
    /// The records, behind `  - ` and `  + `.
    pub records: bool,
    /// The fields that differ, for a changed key.
    pub fields: bool,
    /// Only these fields count, from `--fields`.
    pub only: Option<&'a [String]>,
}

/// Compares `a` and `b` record by record, keyed by `spec`, under `mem`,
/// and prints one line per changed key, followed by what `show` asks for.
/// Exit code 0 when no key changed, 1 otherwise.
///
/// # Errors
///
/// Duplicate or missing keys, a budget too small for one record, or an
/// I/O error.
pub fn run(
    a: &FileSource,
    b: &FileSource,
    paths: [&Path; 2],
    spec: &KeySpec,
    mem: Bytes,
    stats: bool,
    show: Show<'_>,
) -> anyhow::Result<ExitCode> {
    let start = Instant::now();
    let mut out = BufWriter::new(Out {
        inner: io::stdout().lock(),
        failed: None,
    });
    let mut printed = 0u64;
    let budget = crate::budget(mem, false);
    let result = if show.fields {
        compare_fields(a, b, spec, show.only, &budget, |c| {
            printed += 1;
            write_change(&mut out, &c.change.change)?;
            write_fields(&mut out, c)?;
            if show.records {
                write_record_pair(&mut out, c)?;
            }
            Ok(())
        })
    } else if show.records {
        let mut reader = RecordReader::new(&budget, RECORD_BUF)?;
        compare_with_ranges(a, b, spec, &budget, |c| {
            printed += 1;
            write_change(&mut out, &c.change)?;
            write_records(&mut out, c, &mut reader, [a, b])
        })
    } else {
        compare(a, b, spec, &budget, |c| {
            printed += 1;
            write_change(&mut out, c)
        })
    };
    let flushed = out.flush();
    if let Some(e) = out.get_mut().failed.take().or(flushed.err()) {
        return Err(Error::Write(e).into());
    }
    let report = result.map_err(|e| incomplete(with_path(e, paths), printed))?;
    if stats {
        eprintln!("{}", peak_heap(mem));
        eprintln!("time {:.2} s", start.elapsed().as_secs_f64());
        eprintln!(
            "passes {}, records {} in a and {} in b",
            report.passes, report.records_a, report.records_b
        );
    }
    Ok(if report.changes == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// Standard output that keeps its first error. The core reports an error
/// from the callback as a failed read, and with --show-records the
/// callback also reads records back, so only the writer can tell a failed
/// write from a failed read.
struct Out<W> {
    inner: W,
    failed: Option<io::Error>,
}

impl<W: Write> Write for Out<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf).inspect_err(|e| self.keep(e))
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush().inspect_err(|e| self.keep(e))
    }
}

impl<W> Out<W> {
    fn keep(&mut self, e: &io::Error) {
        self.failed
            .get_or_insert_with(|| io::Error::new(e.kind(), e.to_string()));
    }
}

/// Names the file as well as the side when an error points into one:
/// rift-core knows only which side it read.
fn with_path(err: Error, [a, b]: [&Path; 2]) -> anyhow::Error {
    let path = |side| match side {
        Side::A => crate::source::shown(a),
        Side::B => crate::source::shown(b),
    };
    match err {
        Error::Keyed(KeyError::NoColumn { side, name }) => {
            anyhow::anyhow!("{side} ({}) has no column {name:?}", path(side))
        }
        Error::Keyed(KeyError::NoKey { side, line }) => {
            anyhow::anyhow!("line {} of {side} ({}) has no key", line.0 + 1, path(side))
        }
        Error::Keyed(KeyError::TooLong { side, line, cap }) => anyhow::anyhow!(
            "line {} of {side} ({}) starts a record longer than {cap} bytes, the most key mode holds",
            line.0 + 1,
            path(side)
        ),
        e => e.into(),
    }
}

/// A pass prints its changes before a later pass can find a duplicate key,
/// so an error after the first printed line leaves partial output.
fn incomplete(err: anyhow::Error, printed: u64) -> anyhow::Error {
    if printed == 0 {
        err
    } else {
        err.context("the changes printed above are incomplete and must be discarded")
    }
}

/// `~KEY<tab>A<tab>B`, `-KEY<tab>A<tab>-` or `+KEY<tab>-<tab>B`, lines
/// counted from 1, with `TABLE:` before the key of a `COPY` row. Tabs,
/// line breaks and backslashes are escaped so a change stays on one line,
/// and a colon in the table name is written `\:`, so the first colon that
/// is not part of an escape ends the name.
fn write_change(out: &mut impl Write, c: &KeyedChange) -> io::Result<()> {
    out.write_all(match c.kind {
        ChangeKind::Added => b"+",
        ChangeKind::Removed => b"-",
        ChangeKind::Changed => b"~",
    })?;
    if let Some(table) = &c.table {
        for (i, part) in table.split(|&b| b == b':').enumerate() {
            if i > 0 {
                out.write_all(b"\\:")?;
            }
            write_escaped(out, part)?;
        }
        out.write_all(b":")?;
    }
    write_escaped(out, &c.key)?;
    let line = |l: Option<LineNo>| l.map_or_else(|| "-".to_owned(), |l| (l.0 + 1).to_string());
    writeln!(out, "\t{}\t{}", line(c.a), line(c.b))
}

/// Writes `bytes` with tabs, line breaks and backslashes escaped, so they
/// stay on one line.
fn write_escaped(out: &mut impl Write, mut rest: &[u8]) -> io::Result<()> {
    while let Some(i) = rest
        .iter()
        .position(|b| matches!(b, b'\\' | b'\t' | b'\n' | b'\r'))
    {
        out.write_all(&rest[..i])?;
        out.write_all(match rest[i] {
            b'\\' => b"\\\\",
            b'\t' => b"\\t",
            b'\n' => b"\\n",
            _ => b"\\r",
        })?;
        rest = &rest[i + 1..];
    }
    out.write_all(rest)
}

/// For a changed key, `    NAME: OLD -> NEW` per field that differs,
/// four spaces in, so a field line never looks like a change line or a
/// record line. A value longer than 64 bytes prints as a window of at
/// most 64 bytes around its first difference from the other side, with
/// `...` on a cut end; `(none)` stands for a missing field.
fn write_fields(out: &mut impl Write, c: &FieldChange<'_>) -> io::Result<()> {
    if c.change.change.kind != ChangeKind::Changed {
        return Ok(());
    }
    let Some(fields) = c.fields() else {
        return out.write_all(b"    (record not split into fields)\n");
    };
    let mut any = false;
    for f in fields {
        any = true;
        out.write_all(b"    ")?;
        match f.name {
            FieldName::Number(n) => write!(out, "{n}")?,
            FieldName::Bytes(name) => write_escaped(out, name)?,
        }
        out.write_all(b": ")?;
        write_values(out, f.a, f.b)?;
        out.write_all(b"\n")?;
    }
    if !any {
        out.write_all(b"    (no field differs)\n")?;
    }
    Ok(())
}

/// Prints the two values of a field. When either is cut, both print the
/// 64-byte window that holds their first differing byte, so the cut never
/// hides the change.
fn write_values(out: &mut impl Write, a: Option<&[u8]>, b: Option<&[u8]>) -> io::Result<()> {
    let start = match (a, b) {
        (Some(a), Some(b)) if a.len() > VALUE_CUT || b.len() > VALUE_CUT => window_start(a, b),
        _ => 0,
    };
    write_value(out, a, start)?;
    out.write_all(b" -> ")?;
    write_value(out, b, start)
}

/// Where the window of a pair of values starts: 0 while the first
/// difference is in the first 48 bytes, else 16 bytes of context after it.
/// Moved to a character boundary of the shared prefix, so neither side
/// starts or shows the difference inside a character.
fn window_start(a: &[u8], b: &[u8]) -> usize {
    let continuation = |v: &[u8], i: usize| v.get(i).is_some_and(|c| c & 0xC0 == 0x80);
    let mut diff = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    while diff > 0 && (continuation(a, diff) || continuation(b, diff)) {
        diff -= 1;
    }
    let mut start = (diff + 16).saturating_sub(VALUE_CUT);
    while start < diff && continuation(a, start) {
        start += 1;
    }
    start
}

/// `value` from byte `start`, at most [`VALUE_CUT`] bytes, with `...` on
/// each end that was cut; the end stays on a character boundary when the
/// bytes are UTF-8. `(none)` stands for a missing field.
fn write_value(out: &mut impl Write, value: Option<&[u8]>, start: usize) -> io::Result<()> {
    let Some(v) = value else {
        return out.write_all(b"(none)");
    };
    let start = start.min(v.len());
    let mut end = v.len().min(start + VALUE_CUT);
    if end < v.len() {
        // A character cut in two at the end: stop before it.
        if let Err(e) = std::str::from_utf8(&v[start..end])
            && e.error_len().is_none()
        {
            end = start + e.valid_up_to();
        }
    }
    if start > 0 {
        out.write_all(b"...")?;
    }
    write_escaped(out, &v[start..end])?;
    if end < v.len() {
        out.write_all(b"...")?;
    }
    Ok(())
}

/// The records of a field change, as [`write_records`] prints them.
fn write_record_pair(out: &mut impl Write, c: &FieldChange<'_>) -> io::Result<()> {
    for (record, prefix) in [(c.a_record, b"  - "), (c.b_record, b"  + ")] {
        let Some(record) = record else { continue };
        out.write_all(prefix)?;
        write_prefixed(out, prefix, record)?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// Writes `chunk`, putting `prefix` after each of its line breaks.
fn write_prefixed(out: &mut impl Write, prefix: &[u8], mut chunk: &[u8]) -> io::Result<()> {
    while let Some(i) = memchr::memchr(b'\n', chunk) {
        out.write_all(&chunk[..=i])?;
        out.write_all(prefix)?;
        chunk = &chunk[i + 1..];
    }
    out.write_all(chunk)
}

/// The records of `c`, A's then B's, each line behind `  - ` or `  + `.
/// A change line never starts with a space, and a record's own bytes,
/// line breaks and quotes included, print as the file has them.
fn write_records<S: Source + ?Sized>(
    out: &mut impl Write,
    c: &RangedChange,
    reader: &mut RecordReader,
    [a, b]: [&S; 2],
) -> io::Result<()> {
    for (src, range, prefix) in [(a, &c.a_range, b"  - "), (b, &c.b_range, b"  + ")] {
        let Some(range) = range else { continue };
        out.write_all(prefix)?;
        reader.read(src, range.clone(), |chunk| {
            write_prefixed(out, prefix, chunk)
        })?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_core::Budget;

    #[test]
    fn an_error_after_printed_changes_says_to_discard_them() {
        let err = || anyhow::anyhow!("duplicate keys: 7");
        assert_eq!(format!("{:#}", incomplete(err(), 0)), "duplicate keys: 7");
        assert_eq!(
            format!("{:#}", incomplete(err(), 3)),
            "the changes printed above are incomplete and must be discarded: duplicate keys: 7"
        );
    }

    fn change(key: &[u8], kind: ChangeKind, a: Option<u64>, b: Option<u64>) -> KeyedChange {
        KeyedChange {
            key: key.to_vec(),
            kind,
            a: a.map(LineNo),
            b: b.map(LineNo),
            table: None,
        }
    }

    #[test]
    fn every_line_of_a_record_gets_its_prefix_across_buffer_edges() {
        let (a, b): (&[u8], &[u8]) = (b"k,\"ab\n\ncd\"\n", b"k,\"x\"\"\n\"\n");
        let c = RangedChange {
            change: change(b"k", ChangeKind::Changed, Some(0), Some(0)),
            a_range: Some(0..10),
            b_range: Some(0..8),
        };
        let budget = Budget::new(Bytes(1 << 20));
        for size in 1..12 {
            let mut reader = RecordReader::new(&budget, size).unwrap();
            let mut out = Vec::new();
            write_records(&mut out, &c, &mut reader, [a, b]).unwrap();
            assert_eq!(
                String::from_utf8(out).unwrap(),
                "  - k,\"ab\n  - \n  - cd\"\n  + k,\"x\"\"\n  + \"\n",
                "buffer of {size}"
            );
        }
    }

    #[test]
    fn changes_print_one_line_each() {
        let mut out = Vec::new();
        for c in [
            change(b"7", ChangeKind::Changed, Some(0), Some(9)),
            change(b"\"a\tb\nc\\\"", ChangeKind::Removed, Some(4), None),
            change(b"8", ChangeKind::Added, None, Some(2)),
        ] {
            write_change(&mut out, &c).unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "~7\t1\t10\n-\"a\\tb\\nc\\\\\"\t5\t-\n+8\t-\t3\n"
        );
    }

    #[test]
    fn copy_keys_name_their_table_with_its_colons_escaped() {
        let mut out = Vec::new();
        for (table, key) in [
            (&b"public.item"[..], &b"1"[..]),
            (b"public.\"a:b\tc\"", b"2:3"),
        ] {
            let mut c = change(key, ChangeKind::Changed, Some(0), Some(1));
            c.table = Some(table.into());
            write_change(&mut out, &c).unwrap();
        }
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "~public.item:1\t1\t2\n~public.\"a\\:b\\tc\":2:3\t1\t2\n"
        );
    }
}
