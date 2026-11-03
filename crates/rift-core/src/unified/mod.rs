//! Streaming unified diff writer.
//!
//! [`write_diff`] turns a script into the output of `diff -u`: hunks with
//! `context` lines around each change, merged when the equal run between
//! two changes is at most `2 * context` lines, `\ No newline at end of
//! file` after an unterminated last line, bytes of every line passed
//! through untouched (so CRLF stays CRLF), and `---`/`+++` headers with
//! the name and mtime formatted like GNU diff.
//!
//! An approximate hunk is marked by the word `approx` after the closing
//! `@@` of its header, the slot where `diff -p` prints a function name.
//! `patch` ignores that slot, so the diff applies exactly as it would
//! without the mark.
//!
//! Memory: the 1 MiB output buffer, the edits of the open hunk (24 bytes
//! each) and one line buffer of at most 1024 lines, all in the Refine half;
//! the caller holds the reservation. Lines are written straight from the
//! line buffer.

use std::io::{self, BufWriter, Write};
use std::iter::Peekable;

use crate::edit::{Edit, Hunk};
use crate::error::Result;
use crate::lines::{LineAccess, LineBuf};
use crate::script::{Cursor, for_each_line};
use crate::types::{Bytes, LineNo};

/// Context lines around each change by default, as in `diff -u`.
pub const DEFAULT_CONTEXT: u64 = 3;

/// A point in time for a file header, as `diff -u` prints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Timestamp {
    /// Seconds since the Unix epoch.
    pub secs: i64,
    /// Nanoseconds within the second.
    pub nanos: u32,
    /// Local offset from UTC in seconds, east positive.
    pub utc_offset: i32,
}

/// What goes after `---` or `+++`: `name`, then a tab and the mtime when
/// there is one (`diff --label` prints the name alone).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Label<'a> {
    /// File name as given on the command line.
    pub name: &'a [u8],
    /// Modification time, none for a label without a file.
    pub mtime: Option<Timestamp>,
}

/// Output settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Options<'a> {
    /// Equal lines shown around each change.
    pub context: u64,
    /// Header of the old file.
    pub old: Label<'a>,
    /// Header of the new file.
    pub new: Label<'a>,
}

/// Whether the files differ, for the exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Verdict {
    /// The script has no change; nothing was written.
    Identical,
    /// At least one hunk was written.
    Different,
}

/// Exit code as `diff` defines it: 0 identical, 1 different, 2 error.
#[must_use]
pub fn exit_code<E>(result: &std::result::Result<Verdict, E>) -> i32 {
    match result {
        Ok(Verdict::Identical) => 0,
        Ok(Verdict::Different) => 1,
        Err(_) => 2,
    }
}

/// Writes `hunks` as a unified diff of `a` against `b` to `out`.
///
/// `out` is wrapped in a 1 MiB `BufWriter` and flushed before returning.
/// The script is validated while it is walked; on an invalid script the
/// output stops mid-way and the error says where.
///
/// # Errors
///
/// `InvalidScript`, `LineOutOfRange` or `Io` from the sources, `Io` from
/// `out`.
pub fn write_diff<'h>(
    a: &impl LineAccess,
    b: &impl LineAccess,
    hunks: impl IntoIterator<Item = &'h Hunk>,
    opts: &Options<'_>,
    out: impl Write,
) -> Result<Verdict> {
    let edits = hunks
        .into_iter()
        .flat_map(|h| h.edits.iter().map(move |&e| (e, h.approx)));
    let edits = Merged {
        inner: edits.peekable(),
    };
    let mut w = Writer {
        a,
        b,
        opts,
        out: BufWriter::with_capacity(Bytes::mib(1).as_usize(), out),
        buf: LineBuf::new(),
        hunk: Vec::new(),
        start: Cursor::default(),
        has_change: false,
        approx: false,
        wrote_header: false,
    };
    let mut cursor = Cursor::default();
    let ctx = opts.context;
    for (edit, approx, last) in edits {
        let at = cursor;
        cursor.step(edit)?;
        let Edit::Equal { a, b, len } = edit else {
            w.push(at, edit);
            w.has_change = true;
            w.approx |= approx;
            continue;
        };
        if w.has_change {
            // Inside a hunk: a run of at most 2 * ctx lines between two
            // changes is context; a longer one, or the last, closes it.
            if !last && len <= ctx.saturating_mul(2) {
                w.push(at, edit);
                continue;
            }
            w.push(
                at,
                Edit::Equal {
                    a,
                    b,
                    len: len.min(ctx),
                },
            );
            w.flush()?;
        }
        if !last {
            w.lead(a, b, len);
        }
    }
    if w.has_change {
        w.flush()?;
    }
    cursor.finish(a.line_count(), b.line_count())?;
    w.out.flush()?;
    Ok(if w.wrote_header {
        Verdict::Different
    } else {
        Verdict::Identical
    })
}

/// Coalesces consecutive contiguous `Equal` edits, so the gap between two
/// changes is seen as one run however the hunks split it, and flags the
/// last edit.
struct Merged<I: Iterator<Item = (Edit, bool)>> {
    inner: Peekable<I>,
}

impl<I: Iterator<Item = (Edit, bool)>> Iterator for Merged<I> {
    type Item = (Edit, bool, bool);

    fn next(&mut self) -> Option<Self::Item> {
        let (mut edit, approx) = self.inner.next()?;
        if let Edit::Equal { a, b, len } = &mut edit {
            while let Some((
                Edit::Equal {
                    a: a2,
                    b: b2,
                    len: len2,
                },
                _,
            )) = self.inner.peek()
            {
                if *a2 != *a + *len || *b2 != *b + *len {
                    break;
                }
                *len += len2;
                self.inner.next();
            }
        }
        let last = self.inner.peek().is_none();
        Some((edit, approx, last))
    }
}

struct Writer<'o, A, B, W: Write> {
    a: &'o A,
    b: &'o B,
    opts: &'o Options<'o>,
    out: BufWriter<W>,
    buf: LineBuf,
    /// Edits of the open hunk, context included.
    hunk: Vec<Edit>,
    /// Where the open hunk starts in both files.
    start: Cursor,
    has_change: bool,
    approx: bool,
    wrote_header: bool,
}

impl<A: LineAccess, B: LineAccess, W: Write> Writer<'_, A, B, W> {
    /// Opens a hunk with the last `context` lines of an equal run, replacing
    /// any earlier lead that no change followed.
    fn lead(&mut self, a: LineNo, b: LineNo, len: u64) {
        self.hunk.clear();
        let keep = len.min(self.opts.context);
        if keep > 0 {
            let skip = len - keep;
            let (a, b) = (a + skip, b + skip);
            self.push(Cursor { a, b }, Edit::Equal { a, b, len: keep });
        }
    }

    /// Appends `edit`, which starts at `at` in both files, to the open hunk.
    fn push(&mut self, at: Cursor, edit: Edit) {
        if self.hunk.is_empty() {
            self.start = at;
        }
        self.hunk.push(edit);
    }

    /// Writes the open hunk and resets for the next one.
    fn flush(&mut self) -> Result<()> {
        if !self.wrote_header {
            write_label(&mut self.out, b"--- ", &self.opts.old)?;
            write_label(&mut self.out, b"+++ ", &self.opts.new)?;
            self.wrote_header = true;
        }
        let (mut len_a, mut len_b) = (0, 0);
        for edit in &self.hunk {
            match *edit {
                Edit::Equal { len, .. } => {
                    len_a += len;
                    len_b += len;
                }
                Edit::Delete { len, .. } => len_a += len,
                Edit::Insert { len, .. } => len_b += len,
            }
        }
        self.out.write_all(b"@@ -")?;
        write_range(&mut self.out, self.start.a, len_a)?;
        self.out.write_all(b" +")?;
        write_range(&mut self.out, self.start.b, len_b)?;
        self.out.write_all(if self.approx {
            b" @@ approx\n"
        } else {
            b" @@\n"
        })?;
        // A run of changes with no equal line between them is one change
        // block, printed as GNU diff does: every `-` line, then every `+`.
        let mut hunk = std::mem::take(&mut self.hunk);
        let is_equal = |e: &Edit| matches!(e, Edit::Equal { .. });
        let is_insert = |e: &Edit| matches!(e, Edit::Insert { .. });
        for block in hunk.chunk_by(|x, y| is_equal(x) == is_equal(y)) {
            for &edit in block.iter().filter(|e| !is_insert(e)) {
                self.write_lines(edit)?;
            }
            for &edit in block.iter().filter(|e| is_insert(e)) {
                self.write_lines(edit)?;
            }
        }
        hunk.clear();
        self.hunk = hunk;
        self.has_change = false;
        self.approx = false;
        Ok(())
    }

    /// Writes the lines of one edit with its prefix.
    fn write_lines(&mut self, edit: Edit) -> Result<()> {
        let (src, prefix, start, len): (&dyn LineAccess, _, _, _) = match edit {
            Edit::Equal { a, len, .. } => (self.a, b" ", a, len),
            Edit::Delete { a, len } => (self.a, b"-", a, len),
            Edit::Insert { b, len } => (self.b, b"+", b, len),
        };
        let out = &mut self.out;
        for_each_line(&src, start..start + len, &mut self.buf, |line| {
            out.write_all(prefix)?;
            out.write_all(line)?;
            if line.last() != Some(&b'\n') {
                out.write_all(b"\n\\ No newline at end of file\n")?;
            }
            Ok(())
        })
    }
}

/// `start,len` as a hunk header prints it: 1-based, `,len` omitted when 1,
/// and the 0-based start (the line before) when the range is empty.
fn write_range(out: &mut impl Write, start: LineNo, len: u64) -> io::Result<()> {
    let mut n = itoa::Buffer::new();
    if len == 0 {
        out.write_all(n.format(start.0).as_bytes())?;
        out.write_all(b",0")
    } else {
        out.write_all(n.format(start.0 + 1).as_bytes())?;
        if len != 1 {
            out.write_all(b",")?;
            out.write_all(n.format(len).as_bytes())?;
        }
        Ok(())
    }
}

fn write_label(out: &mut impl Write, prefix: &[u8], label: &Label<'_>) -> io::Result<()> {
    out.write_all(prefix)?;
    out.write_all(label.name)?;
    if let Some(ts) = label.mtime {
        out.write_all(b"\t")?;
        write_timestamp(out, ts)?;
    }
    out.write_all(b"\n")
}

/// `YYYY-MM-DD HH:MM:SS.nnnnnnnnn +HHMM` in the timestamp's own offset.
fn write_timestamp(out: &mut impl Write, ts: Timestamp) -> io::Result<()> {
    let local = ts.secs + i64::from(ts.utc_offset);
    let (y, m, d) = civil_from_days(local.div_euclid(86_400));
    let sod = local.rem_euclid(86_400);
    let (hh, mm, ss) = (sod / 3600, sod / 60 % 60, sod % 60);
    let sign = if ts.utc_offset < 0 { '-' } else { '+' };
    let off = ts.utc_offset.unsigned_abs() / 60;
    write!(
        out,
        "{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}.{:09} {sign}{:02}{:02}",
        ts.nanos,
        off / 60,
        off % 60
    )
}

/// Proleptic Gregorian date of a day count since 1970-01-01 (Howard
/// Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests;
