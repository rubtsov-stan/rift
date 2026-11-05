//! Edit script validation and application.
//!
//! This module is the oracle the rest of the crate is tested against:
//! [`validate`] checks the [`Hunk`] invariant, [`apply`] replays a script
//! and the result must equal B byte for byte. It is written for obvious
//! correctness, not speed, and is never on the hot path.

use std::io::Write;
use std::ops::Range;

use crate::edit::{Edit, Hunk};
use crate::error::{Error, Result};
use crate::lines::{LineAccess, LineBuf};
use crate::types::{Bytes, LineNo};

/// Lines fetched per `read_lines` call, which bounds the line buffer to
/// this many lines of the longest line the reader returns whole.
pub(crate) const LINES_PER_READ: u64 = 64;

/// Bytes per piece when a line is too long for `read_lines`.
pub(crate) const PIECE: Bytes = Bytes(64 << 10);

/// Position reached while walking a script.
///
/// Walking the script through [`step`](Cursor::step) checks that every
/// edit starts where the previous one ended; [`finish`](Cursor::finish)
/// checks that the walk ends at the end of both files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Cursor {
    pub a: LineNo,
    pub b: LineNo,
}

impl Cursor {
    /// Checks that `edit` is non-empty and starts at the cursor, then moves
    /// past it.
    pub(crate) fn step(&mut self, edit: Edit) -> Result<()> {
        let (a, b, len_a, len_b) = match edit {
            Edit::Equal { a, b, len } => (Some(a), Some(b), len, len),
            Edit::Delete { a, len } => (Some(a), None, len, 0),
            Edit::Insert { b, len } => (None, Some(b), 0, len),
        };
        if len_a == 0 && len_b == 0 {
            return Err(invalid(format!("{edit:?} has length 0")));
        }
        if a.is_some_and(|a| a != self.a) || b.is_some_and(|b| b != self.b) {
            return Err(invalid(format!(
                "{edit:?} does not start at a={} b={}",
                self.a, self.b
            )));
        }
        let (Some(a), Some(b)) = (self.a.0.checked_add(len_a), self.b.0.checked_add(len_b)) else {
            return Err(invalid(format!("{edit:?} overflows the line numbers")));
        };
        self.a = LineNo(a);
        self.b = LineNo(b);
        Ok(())
    }

    /// Checks that the walk covered exactly `len_a` lines of A and `len_b`
    /// lines of B.
    pub(crate) fn finish(self, len_a: u64, len_b: u64) -> Result<()> {
        if (self.a.0, self.b.0) == (len_a, len_b) {
            Ok(())
        } else {
            Err(invalid(format!(
                "script ends at a={} b={}, files have {len_a} and {len_b} lines",
                self.a, self.b
            )))
        }
    }
}

#[cold]
fn invalid(msg: String) -> Error {
    Error::InvalidScript(msg)
}

/// Checks that `hunks` form one valid script for files of `len_a` and
/// `len_b` lines: edits ordered, contiguous across hunks, non-empty, and
/// covering both files exactly.
///
/// # Errors
///
/// [`Error::InvalidScript`] naming the first offending edit.
pub fn validate<'h>(
    hunks: impl IntoIterator<Item = &'h Hunk>,
    len_a: u64,
    len_b: u64,
) -> Result<()> {
    let mut cursor = Cursor::default();
    for edit in hunks.into_iter().flat_map(|h| h.edits.iter()) {
        cursor.step(*edit)?;
    }
    cursor.finish(len_a, len_b)
}

/// Replays `hunks` against `a`, taking inserted lines from `b`, and streams
/// the result to `out`.
///
/// Equal runs are copied from A, so the output equals B only if the script
/// is right about them; comparing the output with B is the oracle. The
/// script is validated as it is walked, and the output is partial when an
/// error is returned.
///
/// # Errors
///
/// [`Error::InvalidScript`] on a bad script, `LineOutOfRange` or `Io` from
/// the sources, `Io` from `out`.
pub fn apply<'h>(
    a: &impl LineAccess,
    b: &impl LineAccess,
    hunks: impl IntoIterator<Item = &'h Hunk>,
    out: &mut impl Write,
) -> Result<()> {
    let mut cursor = Cursor::default();
    let mut buf = LineBuf::new();
    for &edit in hunks.into_iter().flat_map(|h| h.edits.iter()) {
        cursor.step(edit)?;
        let (src, start, len): (&dyn LineAccess, _, _) = match edit {
            Edit::Equal { a: start, len, .. } => (a, start, len),
            Edit::Insert { b: start, len } => (b, start, len),
            Edit::Delete { .. } => continue,
        };
        for_each_piece(&src, start..start + len, &mut buf, |p, _, _| {
            out.write_all(p)
        })?;
    }
    cursor.finish(a.line_count(), b.line_count())
}

/// Calls `f(piece, first, last)` on the bytes of every line of `range`, in
/// order: once per line with both flags set, or, for a line the reader
/// cannot return whole, once per piece of at most [`PIECE`] bytes, `first`
/// on the first and `last` on the one that ends the line. Lines are read
/// [`LINES_PER_READ`] at a time into `buf`.
pub(crate) fn for_each_piece(
    src: &(impl LineAccess + ?Sized),
    range: Range<LineNo>,
    buf: &mut LineBuf,
    mut f: impl FnMut(&[u8], bool, bool) -> std::io::Result<()>,
) -> Result<()> {
    let mut at = range.start;
    while at < range.end {
        let end = at + (range.end - at).min(LINES_PER_READ);
        let long = match src.read_lines(at..end, buf) {
            Ok(()) => end,
            Err(Error::LineTooLong { line, .. }) if (at..end).contains(&line) => {
                src.read_lines(at..line, buf)?;
                line
            }
            Err(e) => return Err(e),
        };
        for line in buf.iter() {
            f(line, true, true)?;
        }
        if long < end {
            let mut from = 0;
            loop {
                let done = src.read_line_part(long, Bytes(from), PIECE, buf)?;
                let piece = buf.line(0);
                f(piece, from == 0, done)?;
                if done {
                    break;
                }
                from += piece.len() as u64;
            }
            at = long + 1;
        } else {
            at = end;
        }
    }
    Ok(())
}

/// Builds random pairs of files together with a script relating them, for
/// property tests in this module and in `unified`.
#[cfg(test)]
pub(crate) mod testgen {
    use proptest::prelude::*;

    use crate::edit::{Edit, Hunk};
    use crate::types::LineNo;

    /// A pair of files and a script that is valid for them by construction.
    #[derive(Debug, Clone)]
    pub(crate) struct Case {
        pub a: Vec<u8>,
        pub b: Vec<u8>,
        pub hunks: Vec<Hunk>,
    }

    /// Few distinct lines so repeats are common; one is empty, one is CRLF.
    const LINES: [&[u8]; 5] = [b"a\n", b"b\n", b"c\n", b"\n", b"x\r\n"];

    /// One random operation: kind, run length, which line to use, and
    /// whether it opens a new hunk.
    type Op = (u8, u8, u8, bool);

    /// Random cases of up to `max_ops` operations. `crlf` false restricts
    /// lines to `\n` terminators.
    pub(crate) fn cases(max_ops: usize, crlf: bool) -> impl Strategy<Value = Case> {
        let lines = u8::try_from(if crlf { LINES.len() } else { LINES.len() - 1 })
            .expect("fewer than 256 sample lines");
        (
            prop::collection::vec((0..3u8, 1..5u8, 0..lines, any::<bool>()), 0..max_ops),
            any::<bool>(),
            any::<bool>(),
            any::<bool>(),
        )
            .prop_map(|(ops, approx, strip_a, strip_b)| build(&ops, approx, strip_a, strip_b))
    }

    fn build(ops: &[Op], approx: bool, strip_a: bool, strip_b: bool) -> Case {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let (mut la, mut lb) = (0u64, 0u64);
        let mut hunks: Vec<Hunk> = Vec::new();
        // Whether the last line of each file came from an Equal run, which
        // decides whether its terminator can be dropped.
        let (mut a_from_equal, mut b_from_equal) = (false, false);
        for &(kind, len, line, new_hunk) in ops {
            let len = u64::from(len);
            let line = LINES[usize::from(line)];
            let (in_a, in_b) = (kind != 2, kind != 1);
            let edit = match kind {
                0 => Edit::Equal {
                    a: LineNo(la),
                    b: LineNo(lb),
                    len,
                },
                1 => Edit::Delete { a: LineNo(la), len },
                _ => Edit::Insert { b: LineNo(lb), len },
            };
            if in_a {
                a_from_equal = in_b;
                a.extend(line.repeat(usize::try_from(len).expect("len < 5")));
                la += len;
            }
            if in_b {
                b_from_equal = in_a;
                b.extend(line.repeat(usize::try_from(len).expect("len < 5")));
                lb += len;
            }
            if new_hunk || hunks.is_empty() {
                hunks.push(Hunk {
                    edits: Vec::new(),
                    approx: approx && hunks.len().is_multiple_of(2),
                });
            }
            hunks.last_mut().expect("pushed above").edits.push(edit);
        }
        // An unterminated line is only valid at the end of a file, and an
        // Equal run must keep both copies identical: if the last lines of A
        // and B are the same Equal run, strip both or neither.
        if a_from_equal && b_from_equal && last_is_equal(&hunks) {
            strip_newline(&mut a, strip_a);
            strip_newline(&mut b, strip_a);
        } else {
            strip_newline(&mut a, strip_a && !a_from_equal);
            strip_newline(&mut b, strip_b && !b_from_equal);
        }
        Case { a, b, hunks }
    }

    fn last_is_equal(hunks: &[Hunk]) -> bool {
        matches!(
            hunks.last().and_then(|h| h.edits.last()),
            Some(Edit::Equal { .. })
        )
    }

    /// Drops the final `\n` unless that would delete the line itself.
    fn strip_newline(file: &mut Vec<u8>, strip: bool) {
        let ends_in_empty_line = file.len() < 2 || file[file.len() - 2] == b'\n';
        if strip && file.last() == Some(&b'\n') && !ends_in_empty_line {
            file.pop();
        }
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::testgen::{Case, cases};
    use super::*;
    use crate::lines::MemLines;

    fn eq(a: u64, b: u64, len: u64) -> Edit {
        Edit::Equal {
            a: LineNo(a),
            b: LineNo(b),
            len,
        }
    }

    fn hunk(edits: Vec<Edit>) -> Hunk {
        Hunk {
            edits,
            approx: false,
        }
    }

    fn apply_mem(case: &Case) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        apply(
            &MemLines::new(&case.a),
            &MemLines::new(&case.b),
            &case.hunks,
            &mut out,
        )?;
        Ok(out)
    }

    #[test]
    fn empty_script_covers_empty_files_only() {
        validate(&[], 0, 0).unwrap();
        assert!(matches!(validate(&[], 1, 0), Err(Error::InvalidScript(_))));
    }

    #[test]
    fn rejects_gap_overlap_empty_edit_and_short_coverage() {
        let del = |a, len| Edit::Delete { a: LineNo(a), len };
        let ins = |b, len| Edit::Insert { b: LineNo(b), len };
        validate(&[hunk(vec![eq(0, 0, 2), del(2, 1), ins(2, 1)])], 3, 3).unwrap();
        for (script, len_a, len_b) in [
            (vec![eq(0, 0, 2), del(3, 1)], 4, 2),
            (vec![eq(0, 0, 2), del(1, 1)], 2, 2),
            (vec![eq(0, 0, 2), ins(1, 1)], 2, 3),
            (vec![eq(0, 0, 0)], 0, 0),
            (vec![eq(0, 0, 2)], 3, 2),
            (vec![eq(0, 0, 2)], 2, 1),
        ] {
            assert!(
                matches!(
                    validate(&[hunk(script.clone())], len_a, len_b),
                    Err(Error::InvalidScript(_))
                ),
                "{script:?} accepted"
            );
        }
    }

    #[test]
    fn contiguity_is_checked_across_hunks() {
        let h1 = hunk(vec![eq(0, 0, 1)]);
        let h2 = hunk(vec![Edit::Delete {
            a: LineNo(1),
            len: 1,
        }]);
        let h3 = hunk(vec![Edit::Delete {
            a: LineNo(2),
            len: 1,
        }]);
        validate([&h1, &h2], 2, 1).unwrap();
        assert!(validate([&h1, &h3], 3, 1).is_err());
    }

    #[test]
    fn apply_copies_equal_from_a_and_inserts_from_b() {
        let case = Case {
            a: b"a\nb\nc".to_vec(),
            b: b"a\nX\r\nc".to_vec(),
            hunks: vec![hunk(vec![
                eq(0, 0, 1),
                Edit::Delete {
                    a: LineNo(1),
                    len: 1,
                },
                Edit::Insert {
                    b: LineNo(1),
                    len: 1,
                },
                eq(2, 2, 1),
            ])],
        };
        assert_eq!(apply_mem(&case).unwrap(), case.b);
    }

    #[test]
    fn apply_rejects_script_that_stops_early() {
        let case = Case {
            a: b"a\nb\n".to_vec(),
            b: b"a\nb\n".to_vec(),
            hunks: vec![hunk(vec![eq(0, 0, 1)])],
        };
        assert!(matches!(apply_mem(&case), Err(Error::InvalidScript(_))));
    }

    #[test]
    fn for_each_line_batches_without_losing_lines() {
        let data: Vec<u8> = (0..3000)
            .flat_map(|i| format!("{i}\n").into_bytes())
            .collect();
        let src = MemLines::new(&data);
        let mut buf = LineBuf::new();
        let mut out = Vec::new();
        for_each_piece(&src, LineNo(1)..LineNo(2999), &mut buf, |l, first, last| {
            assert!(first && last);
            out.extend_from_slice(l);
            Ok(())
        })
        .unwrap();
        assert_eq!(out, data[2..data.len() - 5]);
        assert!(u64::try_from(buf.len()).unwrap() <= LINES_PER_READ);
    }

    #[test]
    fn lines_longer_than_the_reader_arrive_in_pieces() {
        use crate::budget::{Budget, Half};
        use crate::scan::{ScanConfig, scan};

        let mut data = Vec::new();
        for i in 0..200 {
            if i % 50 == 7 {
                data.extend(std::iter::repeat_n(b'x', 200_000));
            }
            data.extend_from_slice(format!("{i}\n").as_bytes());
        }
        data.extend(std::iter::repeat_n(b'y', 70_000));
        let budget = Budget::new(Bytes::mib(64));
        let cfg = ScanConfig::for_pair(Bytes(data.len() as u64), Bytes(0), &budget).0;
        let scanned = scan(&data[..], &budget, &cfg).unwrap();
        let reader = crate::index::IndexedLines::new(
            &data[..],
            scanned.index(),
            &budget,
            Half::Refine,
            Bytes(4096),
        )
        .unwrap();
        let mem = MemLines::new(&data);
        for range in [0..201, 5..9, 7..8, 200..201, 150..200] {
            let (mut out, mut lines, mut open) = (Vec::new(), 0, false);
            let mut buf = LineBuf::new();
            let r = LineNo(range.start)..LineNo(range.end);
            for_each_piece(&reader, r, &mut buf, |p, first, last| {
                assert!(p.len() as u64 <= PIECE.0.max(4096));
                assert_eq!(first, !open, "first marks a new line");
                open = !last;
                lines += u64::from(last);
                out.extend_from_slice(p);
                Ok(())
            })
            .unwrap();
            let want: Vec<u8> = range
                .clone()
                .flat_map(|i| mem.line(i).unwrap().to_vec())
                .collect();
            assert_eq!(out, want, "{range:?}");
            assert_eq!(lines, range.end - range.start);
        }
    }

    proptest! {
        #[test]
        fn random_scripts_validate_and_apply_to_b(case in cases(24, true)) {
            let (a, b) = (MemLines::new(&case.a), MemLines::new(&case.b));
            validate(&case.hunks, a.line_count(), b.line_count()).unwrap();
            prop_assert_eq!(apply_mem(&case).unwrap(), case.b);
        }
    }
}
