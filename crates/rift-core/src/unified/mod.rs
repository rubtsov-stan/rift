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
//! Memory: the 1 MiB output buffer and one line buffer of at most 64
//! lines, both in the Refine half; the caller holds the reservation. No
//! edit is kept: each unified hunk is walked twice, once for its header and
//! once for its lines, so a hunk of any length costs nothing more. Lines
//! are written straight from the line buffer.

use std::io::{self, BufWriter, Write};
use std::marker::PhantomData;
use std::ops::{Deref, Range};

use crate::edit::{Edit, Hunk};
use crate::error::Result;
use crate::lines::{LineAccess, LineBuf};
use crate::script::{Cursor, for_each_piece};
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

/// Writes the script whose `i`-th hunk `hunk(i)` returns as a unified diff
/// of `a` against `b` to `out`. `hunk` returns `None` past the last hunk.
///
/// The writer keeps no edits: it walks the script twice per unified hunk,
/// once to find where the hunk ends and print its header, once to print
/// its lines, so `hunk` is called again for the hunks a unified hunk spans
/// and must return the same script each time; it is called once per edit
/// read and its result dropped right after. Memory is the 1 MiB output
/// buffer and a line buffer.
///
/// `out` is wrapped in a 1 MiB `BufWriter` and flushed before returning.
/// The script is validated while it is walked; on an invalid script the
/// output stops mid-way and the error says where.
///
/// # Errors
///
/// `InvalidScript`, `LineOutOfRange` or `Io` from the sources, `Io` from
/// `out`, and whatever `hunk` returns.
pub fn write_diff<H: Deref<Target = Hunk>>(
    a: &impl LineAccess,
    b: &impl LineAccess,
    hunk: impl Fn(usize) -> Result<Option<H>>,
    opts: &Options<'_>,
    out: impl Write,
) -> Result<Verdict> {
    let mut w = Writer {
        a,
        b,
        opts,
        out: BufWriter::with_capacity(Bytes::mib(1).as_usize(), out),
        buf: LineBuf::new(),
        wrote_header: false,
    };
    let ctx = opts.context;
    let mut walk = Merged::new(&hunk, At::default());
    let mut cursor = Cursor::default();
    // The last `ctx` lines of the equal run before the next change.
    let mut lead = Cursor::default()..Cursor::default();
    while let Some(m) = walk.next()? {
        let at = cursor;
        cursor.step(m.edit)?;
        if let Edit::Equal { len, .. } = m.edit {
            lead = back(cursor, len.min(ctx))..cursor;
            continue;
        }
        if lead.end != at {
            lead = at..at;
        }
        // First pass: where the hunk ends, whether it is approximate, and
        // the walk positioned after its closing equal run.
        let first = m.at;
        let mut end = cursor;
        let mut approx = m.approx;
        let mut next = None;
        while let Some(m) = walk.next()? {
            cursor.step(m.edit)?;
            match m.edit {
                Edit::Equal { len, .. } if m.last || len > ctx.saturating_mul(2) => {
                    let keep = len.min(ctx);
                    end = Cursor {
                        a: end.a + keep,
                        b: end.b + keep,
                    };
                    next = Some(m);
                    break;
                }
                Edit::Equal { .. } => end = cursor,
                _ => {
                    approx |= m.approx;
                    end = cursor;
                }
            }
        }
        w.header(lead.start, end, approx)?;
        w.lines(Side::Equal, lead.start.a, lead.end.a - lead.start.a)?;
        w.body(&hunk, first, end)?;
        match next {
            Some(m) if !m.last => {
                let Edit::Equal { len, .. } = m.edit else {
                    unreachable!("the first pass stops on an equal run")
                };
                lead = back(cursor, len.min(ctx))..cursor;
            }
            _ => {}
        }
    }
    cursor.finish(a.line_count(), b.line_count())?;
    w.out.flush()?;
    Ok(if w.wrote_header {
        Verdict::Different
    } else {
        Verdict::Identical
    })
}

/// `c` moved `n` lines back in both files.
fn back(c: Cursor, n: u64) -> Cursor {
    Cursor {
        a: LineNo(c.a.0 - n),
        b: LineNo(c.b.0 - n),
    }
}

/// Position of a raw edit: hunk index and edit index within it.
#[derive(Debug, Clone, Copy, Default)]
struct At {
    hunk: usize,
    edit: usize,
}

/// One edit of the merged walk.
#[derive(Debug, Clone, Copy)]
struct Step {
    edit: Edit,
    /// The hunk it came from was approximate.
    approx: bool,
    /// Where it starts in the raw script.
    at: At,
    /// Nothing follows it.
    last: bool,
}

/// Walks the script from a position, coalescing consecutive contiguous
/// `Equal` edits so the gap between two changes is seen as one run however
/// the hunks split it, and flagging the last edit.
///
/// A hunk is fetched again for every edit and dropped at once, so the
/// writer holds no hunk while it waits for the next one or writes lines:
/// a scheduler's cache stays free to evict what the writer has read.
struct Merged<'f, F, H> {
    hunk: &'f F,
    at: At,
    peeked: Option<(Edit, bool, At)>,
    _hunk: PhantomData<H>,
}

impl<'f, F: Fn(usize) -> Result<Option<H>>, H: Deref<Target = Hunk>> Merged<'f, F, H> {
    fn new(hunk: &'f F, at: At) -> Self {
        Self {
            hunk,
            at,
            peeked: None,
            _hunk: PhantomData,
        }
    }

    /// The next raw edit and its hunk's approx flag.
    fn raw(&mut self) -> Result<Option<(Edit, bool, At)>> {
        if let Some(p) = self.peeked.take() {
            return Ok(Some(p));
        }
        loop {
            let Some(h) = (self.hunk)(self.at.hunk)? else {
                return Ok(None);
            };
            if let Some(&edit) = h.edits.get(self.at.edit) {
                let at = self.at;
                self.at.edit += 1;
                return Ok(Some((edit, h.approx, at)));
            }
            self.at = At {
                hunk: self.at.hunk + 1,
                edit: 0,
            };
        }
    }

    fn next(&mut self) -> Result<Option<Step>> {
        let Some((mut edit, approx, at)) = self.raw()? else {
            return Ok(None);
        };
        if let Edit::Equal { a, b, len } = &mut edit {
            while let Some(p) = self.raw()? {
                match p.0 {
                    Edit::Equal {
                        a: a2,
                        b: b2,
                        len: len2,
                    } if a2 == *a + *len && b2 == *b + *len => *len += len2,
                    _ => {
                        self.peeked = Some(p);
                        break;
                    }
                }
            }
        }
        let next = self.raw()?;
        let last = next.is_none();
        self.peeked = next;
        Ok(Some(Step {
            edit,
            approx,
            at,
            last,
        }))
    }
}

#[derive(Clone, Copy)]
enum Side {
    Equal,
    Delete,
    Insert,
}

struct Writer<'o, A, B, W: Write> {
    a: &'o A,
    b: &'o B,
    opts: &'o Options<'o>,
    out: BufWriter<W>,
    buf: LineBuf,
    wrote_header: bool,
}

impl<A: LineAccess, B: LineAccess, W: Write> Writer<'_, A, B, W> {
    /// The file headers before the first hunk, then the `@@` line of a
    /// hunk from `start` to `end`.
    fn header(&mut self, start: Cursor, end: Cursor, approx: bool) -> Result<()> {
        if !self.wrote_header {
            write_label(&mut self.out, b"--- ", &self.opts.old)?;
            write_label(&mut self.out, b"+++ ", &self.opts.new)?;
            self.wrote_header = true;
        }
        self.out.write_all(b"@@ -")?;
        write_range(&mut self.out, start.a, end.a - start.a)?;
        self.out.write_all(b" +")?;
        write_range(&mut self.out, start.b, end.b - start.b)?;
        self.out
            .write_all(if approx { b" @@ approx\n" } else { b" @@\n" })?;
        Ok(())
    }

    /// Second pass: the lines of the hunk from its first change at `first`
    /// to `end`. A run of changes with no equal line between them is one
    /// change block, printed as GNU diff does: every `-` line, then every
    /// `+`. Within a block the deletes are contiguous in A and the inserts
    /// in B, so two ranges describe it.
    fn body<F, H>(&mut self, hunk: &F, first: At, end: Cursor) -> Result<()>
    where
        F: Fn(usize) -> Result<Option<H>>,
        H: Deref<Target = Hunk>,
    {
        let mut walk = Merged::new(hunk, first);
        let (mut dels, mut ins) = (None, None);
        while let Some(m) = walk.next()? {
            match m.edit {
                Edit::Equal { a, b, len } => {
                    self.block(dels.take(), ins.take())?;
                    let len = len.min(end.a - a);
                    self.lines(Side::Equal, a, len)?;
                    if (a + len, b + len) == (end.a, end.b) {
                        return Ok(());
                    }
                }
                Edit::Delete { a, len } => dels.get_or_insert(a..a).end = a + len,
                Edit::Insert { b, len } => ins.get_or_insert(b..b).end = b + len,
            }
        }
        self.block(dels, ins)
    }

    fn block(&mut self, dels: Option<Range<LineNo>>, ins: Option<Range<LineNo>>) -> Result<()> {
        if let Some(r) = dels {
            self.lines(Side::Delete, r.start, r.end - r.start)?;
        }
        if let Some(r) = ins {
            self.lines(Side::Insert, r.start, r.end - r.start)?;
        }
        Ok(())
    }

    /// Writes `len` lines from `start` with the prefix for `side`.
    fn lines(&mut self, side: Side, start: LineNo, len: u64) -> Result<()> {
        let (src, prefix): (&dyn LineAccess, _) = match side {
            Side::Equal => (self.a, b" "),
            Side::Delete => (self.a, b"-"),
            Side::Insert => (self.b, b"+"),
        };
        let out = &mut self.out;
        for_each_piece(
            &src,
            start..start + len,
            &mut self.buf,
            |piece, first, last| {
                if first {
                    out.write_all(prefix)?;
                }
                out.write_all(piece)?;
                if last && piece.last() != Some(&b'\n') {
                    out.write_all(b"\n\\ No newline at end of file\n")?;
                }
                Ok(())
            },
        )
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
