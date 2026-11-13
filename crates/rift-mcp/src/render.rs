//! A segment's edit script as bounded unified-diff text.

use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rift_core::diff::Diff;
use rift_core::{
    Bytes, DiffView, Edit, HunkState, LineAccess, LineBuf, LineNo, Segment, SegmentStatus,
};

use crate::file::FileSource;

/// Longest text of one diff line kept; the rest is replaced by a count.
pub const MAX_LINE_BYTES: u64 = 256;
/// Largest `max_lines` a call may ask for.
pub const MAX_LINES: u64 = 500;
/// Default `max_lines`.
pub const DEFAULT_LINES: u64 = 100;
/// Largest `context` a call may ask for.
pub const MAX_CONTEXT: u64 = 20;
/// Default `context`.
pub const DEFAULT_CONTEXT: u64 = 3;
/// Size of the diff text in one response, whatever `max_lines` says.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// How long a call waits for a segment to be refined.
const HUNK_WAIT: Duration = Duration::from_secs(60);

/// The rendered hunk.
pub struct Rendered {
    /// Header and lines, each ending in a newline.
    pub text: String,
    /// Diff lines in `text`, the header not counted.
    pub shown: u64,
    /// Diff lines the whole hunk has, context included.
    pub total: u64,
    /// Changed lines of A: 0-based start and count, context not included.
    pub a: (u64, u64),
    /// Changed lines of B.
    pub b: (u64, u64),
    /// The script is valid but not minimal.
    pub approx: bool,
}

/// Waits until segment `i` has a script.
fn script(diff: &Diff<FileSource, FileSource>, i: usize) -> Result<Arc<rift_core::Hunk>, String> {
    let seg = &diff.segments()[i];
    let start = seg.a.start.0.saturating_sub(1);
    diff.set_visible(LineNo(start)..LineNo(seg.a.end.0.max(start + 1)));
    let began = Instant::now();
    loop {
        match diff.hunk(i) {
            HunkState::Ready(h) | HunkState::Approx(h) => return Ok(h),
            HunkState::Pending if began.elapsed() > HUNK_WAIT => {
                return Err("the hunk was not computed in time".into());
            }
            HunkState::Pending => std::thread::sleep(Duration::from_millis(1)),
        }
    }
}

/// Lines of segment `i` available as context, if it is an equal segment.
fn neighbour(segs: &[Segment], i: Option<usize>) -> u64 {
    i.and_then(|j| segs.get(j))
        .filter(|s| s.status == SegmentStatus::Equal)
        .map_or(0, Segment::len_a)
}

/// Renders the changes of segment `i` with up to `ctx` lines of context on
/// each side and at most `max_lines` lines and [`MAX_TEXT_BYTES`] bytes of
/// text. Equal lines at the ends of the segment count as context, so the
/// range is the changed lines, not the whole segment.
///
/// # Errors
///
/// A message for the client when the segment is not computed in time or a
/// file cannot be read.
pub fn hunk_text(
    diff: &Diff<FileSource, FileSource>,
    i: usize,
    ctx: u64,
    max_lines: u64,
) -> Result<Rendered, String> {
    let segs = diff.segments();
    let seg = &segs[i];
    let hunk = script(diff, i)?;
    let is_change = |e: &Edit| !matches!(e, Edit::Equal { .. });
    let first = hunk.edits.iter().position(is_change).unwrap_or(0);
    let last = hunk.edits.iter().rposition(is_change).unwrap_or(0);
    let edits = &hunk.edits[first..=last];
    let sum = |es: &[Edit]| es.iter().map(edit_len).sum::<u64>();
    let lead = sum(&hunk.edits[..first]);
    let trail = sum(&hunk.edits[last + 1..]);
    let pre = ctx.min(lead + neighbour(segs, i.checked_sub(1)));
    let post = ctx.min(trail + neighbour(segs, Some(i + 1)));
    let (a0, b0) = (seg.a.start.0 + lead, seg.b.start.0 + lead);
    let side =
        |skip: fn(&Edit) -> bool| -> u64 { edits.iter().filter(|e| !skip(e)).map(edit_len).sum() };
    let a_len = side(|e| matches!(e, Edit::Insert { .. }));
    let b_len = side(|e| matches!(e, Edit::Delete { .. }));
    let (ac, bc) = (pre + a_len + post, pre + b_len + post);
    // As in `diff -u`, an empty range is named by the line before it.
    let start = |s: u64, count: u64| if count == 0 { s - pre } else { s - pre + 1 };
    let mut text = format!("@@ -{},{ac} +{},{bc} @@", start(a0, ac), start(b0, bc));
    if let Some(reason) = hunk.approx {
        let _ = write!(text, " approx: {}", reason.describe());
    }
    text.push('\n');
    let total = pre + sum(edits) + post;
    let mut out = Out {
        text: &mut text,
        shown: 0,
        max_lines,
        buf: LineBuf::new(),
    };
    let (la, lb) = (diff.lines_a(), diff.lines_b());
    out.run(la, ' ', a0 - pre, pre)?;
    for e in edits {
        match *e {
            Edit::Equal { a, len, .. } => out.run(la, ' ', a.0, len)?,
            Edit::Delete { a, len } => out.run(la, '-', a.0, len)?,
            Edit::Insert { b, len } => out.run(lb, '+', b.0, len)?,
        }
    }
    out.run(la, ' ', a0 + a_len, post)?;
    let shown = out.shown;
    Ok(Rendered {
        text,
        shown,
        total,
        a: (a0, a_len),
        b: (b0, b_len),
        approx: hunk.approx.is_some(),
    })
}

fn edit_len(e: &Edit) -> u64 {
    match *e {
        Edit::Equal { len, .. } | Edit::Delete { len, .. } | Edit::Insert { len, .. } => len,
    }
}

struct Out<'t> {
    text: &'t mut String,
    shown: u64,
    max_lines: u64,
    buf: LineBuf,
}

impl Out<'_> {
    fn full(&self) -> bool {
        self.shown >= self.max_lines || self.text.len() >= MAX_TEXT_BYTES
    }

    /// Appends `len` lines from `lines` starting at `from`, each behind
    /// `mark`, until a bound is hit.
    fn run(
        &mut self,
        lines: &impl LineAccess,
        mark: char,
        from: u64,
        len: u64,
    ) -> Result<(), String> {
        for n in from..from + len {
            if self.full() {
                return Ok(());
            }
            let full = lines
                .read_line_prefix(LineNo(n), Bytes(MAX_LINE_BYTES), &mut self.buf)
                .map_err(|e| format!("read line {}: {e}", n + 1))?;
            let raw = self.buf.line(0);
            let body = raw
                .strip_suffix(b"\n")
                .map_or(raw, |r| r.strip_suffix(b"\r").unwrap_or(r));
            let _ = write!(self.text, "{mark}{}", String::from_utf8_lossy(body));
            if full.0 > raw.len() as u64 {
                let _ = write!(self.text, " [+{} bytes]", full.0 - raw.len() as u64);
            }
            self.text.push('\n');
            self.shown += 1;
        }
        Ok(())
    }
}
