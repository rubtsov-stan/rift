//! `--key`: records compared by a key column, independent of row order.

use std::io::{self, BufWriter, Write};
use std::process::ExitCode;
use std::time::Instant;

use rift_core::keyed::{ChangeKind, KeySpec, KeyedChange, compare};
use rift_core::{Budget, Bytes, LineNo};

use crate::source::FileSource;
use crate::summary::peak_heap;

/// Compares `a` and `b` record by record, keyed by `spec`, under `mem`,
/// and prints one line per changed key. Exit code 0 when no key changed,
/// 1 otherwise.
///
/// # Errors
///
/// Duplicate or missing keys, a budget too small for one record, or an
/// I/O error.
pub fn run(
    a: &FileSource,
    b: &FileSource,
    spec: &KeySpec,
    mem: Bytes,
    stats: bool,
) -> anyhow::Result<ExitCode> {
    let start = Instant::now();
    let mut out = BufWriter::new(io::stdout().lock());
    let mut printed = 0u64;
    let result = compare(a, b, spec, &Budget::new(mem), |c| {
        printed += 1;
        write_change(&mut out, c)
    });
    out.flush()?;
    let report = result.map_err(|e| incomplete(e.into(), printed))?;
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
