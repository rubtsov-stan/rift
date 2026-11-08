//! `--key`: records compared by a key column, independent of row order.

use std::io::{self, BufWriter, Write};
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use rift_core::keyed::{
    ChangeKind, KeyError, KeySpec, KeyedChange, RangedChange, RecordReader, Side, compare,
    compare_with_ranges,
};
use rift_core::{Budget, Bytes, Error, LineNo, Source};

use crate::source::FileSource;
use crate::summary::peak_heap;

/// Read-back buffer for `--show-records`, from the refine half: a record
/// longer than this streams through it.
const RECORD_BUF: usize = 64 << 10;

/// Compares `a` and `b` record by record, keyed by `spec`, under `mem`,
/// and prints one line per changed key, followed by its records when
/// `show_records` is set. Exit code 0 when no key changed, 1 otherwise.
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
    show_records: bool,
) -> anyhow::Result<ExitCode> {
    let start = Instant::now();
    let mut out = BufWriter::new(Out {
        inner: io::stdout().lock(),
        failed: None,
    });
    let mut printed = 0u64;
    let budget = Budget::new(mem);
    let result = if show_records {
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
        Side::A => a.display(),
        Side::B => b.display(),
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
/// counted from 1. Tabs, line breaks and backslashes in the key are
/// escaped so a change stays on one line.
fn write_change(out: &mut impl Write, c: &KeyedChange) -> io::Result<()> {
    out.write_all(match c.kind {
        ChangeKind::Added => b"+",
        ChangeKind::Removed => b"-",
        ChangeKind::Changed => b"~",
    })?;
    let mut rest = &c.key[..];
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
    out.write_all(rest)?;
    let line = |l: Option<LineNo>| l.map_or_else(|| "-".to_owned(), |l| (l.0 + 1).to_string());
    writeln!(out, "\t{}\t{}", line(c.a), line(c.b))
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
        reader.read(src, range.clone(), |mut chunk| {
            while let Some(i) = memchr::memchr(b'\n', chunk) {
                out.write_all(&chunk[..=i])?;
                out.write_all(prefix)?;
                chunk = &chunk[i + 1..];
            }
            out.write_all(chunk)
        })?;
        out.write_all(b"\n")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
