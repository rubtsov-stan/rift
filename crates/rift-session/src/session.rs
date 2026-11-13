//! What a client asks of one session: the summary, the differing
//! segments, refinement state, and excerpts of both files.

use std::borrow::Cow;

use rift_core::diff::Diff;
use rift_core::{
    Bytes, DiffView, Half, HunkState, LineAccess, LineBuf, LineNo, Progress, Reservation, Segment,
    SegmentStatus,
};

use crate::error::Error;
use crate::file::FileSource;
use crate::sessions::{Session, SessionId};

/// Default and largest `context` of an excerpt.
pub const DEFAULT_CONTEXT: u64 = 3;
/// Largest `context` of an excerpt.
pub const MAX_CONTEXT: u64 = 100;
/// Default lines per side of an excerpt.
pub const DEFAULT_LINES: u64 = 1000;
/// Largest lines per side of an excerpt.
pub const MAX_LINES: u64 = 5000;
/// Default bytes of one line's text.
pub const DEFAULT_LINE_BYTES: u64 = 4096;
/// Largest bytes of one line's text.
pub const MAX_LINE_BYTES: u64 = 64 << 10;
/// Default entries of a page of segments or moved blocks.
pub const DEFAULT_PAGE: usize = 100;
/// Largest page of segments or moved blocks.
pub const MAX_PAGE: usize = 1000;

/// Segments of one status.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    /// Segments.
    pub segments: u64,
    /// Their lines in A.
    pub a_lines: u64,
    /// Their lines in B.
    pub b_lines: u64,
}

/// Whether moved blocks were looked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MovesState {
    /// The search ran; the list may be empty.
    Found,
    /// The list filled up and the search stopped, so moves past the last
    /// block listed may be missing.
    Partial,
    /// Move detection was switched off.
    Off,
    /// The budget did not allow the search; an empty list says nothing.
    Skipped,
}

/// Sizes and counts of one diff. Holds no heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// Lines in A.
    pub a_lines: u64,
    /// Lines in B.
    pub b_lines: u64,
    /// Bytes in A.
    pub a_bytes: Bytes,
    /// Bytes in B.
    pub b_bytes: Bytes,
    /// No segment differs.
    pub identical: bool,
    /// Equal segments.
    pub equal: Tally,
    /// Differing segments not known to be approximate: not refined yet, or
    /// refined exactly.
    pub differs: Tally,
    /// Refined segments whose hunk came out approximate, out of
    /// `progress.done`. Grows while refinement runs.
    pub approx: Tally,
    /// `differs` plus `approx` segments.
    pub differing: u64,
    /// Whether moved blocks were looked for.
    pub moves: MovesState,
    /// Moved blocks found.
    pub moved_blocks: u64,
    /// The session's budget.
    pub mem: Bytes,
    /// Bytes of it reserved now, both halves.
    pub reserved: Bytes,
    /// Refinement so far.
    pub progress: Progress,
}

/// Where an excerpt is taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum At {
    /// A segment, by its index in the map.
    Segment(usize),
    /// The segment whose A range holds this line.
    LineA(LineNo),
}

/// What [`Session::excerpt`] is asked for. Values over the caps are
/// served at the caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExcerptRequest {
    /// Where.
    pub at: At,
    /// Lines before and after the segment, at most [`MAX_CONTEXT`].
    pub context: u64,
    /// Lines per side, at least 1 and at most [`MAX_LINES`].
    pub max_lines: u64,
    /// Bytes of one line's text, at least 1 and at most
    /// [`MAX_LINE_BYTES`].
    pub max_line_bytes: u64,
}

impl ExcerptRequest {
    /// A request at `at` with the default context and limits.
    #[must_use]
    pub fn new(at: At) -> Self {
        Self {
            at,
            context: DEFAULT_CONTEXT,
            max_lines: DEFAULT_LINES,
            max_line_bytes: DEFAULT_LINE_BYTES,
        }
    }
}

/// How a line ends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Eol {
    /// `\n`.
    Lf,
    /// `\r\n`.
    CrLf,
    /// The last line of a file without a terminator.
    None,
}

/// One line of an excerpt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The line without its terminator, cut at the request's
    /// `max_line_bytes`, decoded as UTF-8 with each invalid sequence
    /// replaced by U+FFFD. A sequence cut by the limit is dropped.
    pub text: String,
    /// The full length of the line, terminator excluded.
    pub bytes: u64,
    /// How the line ends.
    pub eol: Eol,
    /// `text` holds only part of the line.
    pub truncated: bool,
    /// The bytes shown are not valid UTF-8.
    pub lossy: bool,
    /// The line lies in a moved block on its side. It can be a context
    /// line, when the diff aligned it with another copy.
    pub moved: bool,
}

/// Consecutive lines of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    /// Line number of `lines[0]`.
    pub start: LineNo,
    /// Lines in the window, before the line and byte limits.
    pub total: u64,
    /// The lines shown.
    pub lines: Vec<Line>,
}

impl Column {
    /// Fewer lines are shown than the window has.
    #[must_use]
    pub fn truncated(&self) -> bool {
        (self.lines.len() as u64) < self.total
    }
}

/// Both sides of one segment with context, as [`Session::excerpt`]
/// returns them. Holds its share of the response pool until dropped.
#[derive(Debug)]
pub struct Excerpt {
    /// The segment's index in the map.
    pub segment: usize,
    /// `Approx` when the map says so or the refined script was cut short.
    pub status: SegmentStatus,
    /// Refinement has not finished, so `status` may still turn `Approx`.
    pub pending: bool,
    /// The segment, without context.
    pub range: Segment,
    /// Lines of A.
    pub a: Column,
    /// Lines of B.
    pub b: Column,
    _held: Reservation,
}

impl Excerpt {
    /// Either side shows fewer lines than its window has.
    #[must_use]
    pub fn truncated(&self) -> bool {
        self.a.truncated() || self.b.truncated()
    }
}

const LINE: u64 = size_of::<Line>() as u64;
/// Heap of a line's text per byte shown, at worst: an invalid byte
/// decodes to a three-byte U+FFFD, and the decoder's doubling growth can
/// leave its string at four.
const TEXT_PER_BYTE: u64 = 4;
/// Lines a column makes room for at first; it doubles from there.
const FIRST_LINES_USIZE: usize = 16;
const FIRST_LINES: u64 = FIRST_LINES_USIZE as u64;

impl Session {
    /// The session's id.
    #[must_use]
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// The diff, for what this type does not wrap: `set_visible`,
    /// `progress`, `moved_blocks`, the segment map.
    ///
    /// # Errors
    ///
    /// [`Error::NotReady`] while the session scans or after its scan failed.
    pub fn diff(&self) -> Result<&Diff<FileSource, FileSource>, Error> {
        Ok(&self.built()?.diff)
    }

    /// The session's budget.
    #[must_use]
    pub fn mem(&self) -> Bytes {
        self.mem
    }

    /// Sizes of A and B.
    #[must_use]
    pub fn bytes(&self) -> [Bytes; 2] {
        self.bytes
    }

    /// The differing segments with their indices, in file order. Borrows
    /// the map: a page of them needs no memory of its own.
    ///
    /// # Errors
    ///
    /// [`Error::NotReady`] while the session scans or after its scan failed.
    pub fn differing(&self) -> Result<impl Iterator<Item = (usize, &Segment)>, Error> {
        Ok(self
            .built()?
            .diff
            .segments()
            .iter()
            .enumerate()
            .filter(|(_, s)| s.status != SegmentStatus::Equal))
    }

    /// Sizes and counts. `O(segments)` time, no heap.
    ///
    /// # Errors
    ///
    /// [`Error::NotReady`] while the session scans or after its scan failed.
    pub fn summary(&self) -> Result<Summary, Error> {
        let built = self.built()?;
        let diff = &built.diff;
        let segs = diff.segments();
        let tally = |st: SegmentStatus| {
            segs.iter()
                .filter(|x| x.status == st)
                .fold(Tally::default(), |t, x| Tally {
                    segments: t.segments + 1,
                    a_lines: t.a_lines + x.len_a(),
                    b_lines: t.b_lines + x.len_b(),
                })
        };
        // Approx before progress: both only grow, so approx stays within
        // the refined count reported with it.
        let refined = diff.approx_refined();
        let progress = diff.progress();
        let approx = Tally {
            segments: refined.segments,
            a_lines: refined.a_lines,
            b_lines: refined.b_lines,
        };
        let mapped = tally(SegmentStatus::Differs);
        let differs = Tally {
            segments: mapped.segments - approx.segments,
            a_lines: mapped.a_lines - approx.a_lines,
            b_lines: mapped.b_lines - approx.b_lines,
        };
        let moves = diff.moves();
        let used = |h| built.budget.used(h).0;
        Ok(Summary {
            a_lines: diff.len_a(),
            b_lines: diff.len_b(),
            a_bytes: self.bytes[0],
            b_bytes: self.bytes[1],
            identical: differs.segments + approx.segments == 0,
            equal: tally(SegmentStatus::Equal),
            differs,
            approx,
            differing: differs.segments + approx.segments,
            moves: if moves.disabled() {
                MovesState::Off
            } else if moves.skipped() {
                MovesState::Skipped
            } else if moves.full() {
                MovesState::Partial
            } else {
                MovesState::Found
            },
            moved_blocks: moves.blocks().len() as u64,
            mem: self.mem,
            reserved: Bytes(used(Half::Skeleton) + used(Half::Refine)),
            progress,
        })
    }

    /// The refinement state of segment `i`, queueing it if it has not
    /// started. Never blocks.
    ///
    /// # Errors
    ///
    /// [`Error::NotReady`] while the session scans or after its scan
    /// failed, [`Error::SegmentOutOfRange`], or [`Error::Segment`] with the
    /// error the refinement failed with.
    pub fn hunk_state(&self, i: usize) -> Result<HunkState, Error> {
        self.check_segment(i)?;
        self.built()?
            .diff
            .try_hunk(i)
            .map_err(|error| Error::Segment {
                segment: i,
                mem: self.mem,
                error,
            })
    }

    fn check_segment(&self, index: usize) -> Result<(), Error> {
        let count = self.built()?.diff.segments().len();
        if index < count {
            Ok(())
        } else {
            Err(Error::SegmentOutOfRange { index, count })
        }
    }

    /// Lines of both files around one segment, read straight from the
    /// files: it never waits for refinement, and asking queues the
    /// segment for it.
    ///
    /// A differing segment is shown from its first line; an equal one
    /// reached through [`At::LineA`] from that line, since its lines match
    /// one for one. Each side shows at most `max_lines` lines of at most
    /// `max_line_bytes` bytes, and both sides together fit what the
    /// response pool had free when the call began. Memory: the lines, their
    /// text and one line buffer, held by the returned excerpt until it is
    /// dropped. Time: one positional read per line shown, two for a
    /// truncated one, plus `O(moved blocks)`.
    ///
    /// [`Sessions::get`]: crate::Sessions::get
    ///
    /// # Errors
    ///
    /// [`Error::NotReady`] while the session scans or after its scan
    /// failed, [`Error::NoResponsePool`] for a session from
    /// [`Sessions::get`], [`Error::SegmentOutOfRange`] or [`Error::LineOutOfRange`]
    /// for a position past the end, [`Error::ResponsesBusy`] when other excerpts
    /// hold the pool, [`Error::Segment`] when a read or the segment's
    /// refinement failed.
    pub fn excerpt(&self, req: &ExcerptRequest) -> Result<Excerpt, Error> {
        let built = self.built()?;
        let pool = &built.pool.as_ref().ok_or(Error::NoResponsePool)?.budget;
        let diff = &built.diff;
        let segs = diff.segments();
        let (i, from) = match req.at {
            At::Segment(i) => (i, None),
            At::LineA(line) => {
                let count = diff.len_a();
                if line.0 >= count {
                    return Err(Error::LineOutOfRange { line, count });
                }
                // Segments tile A, so the first one ending after the line
                // starts at or before it.
                (segs.partition_point(|s| s.a.end <= line), Some(line))
            }
        };
        self.check_segment(i)?;
        let seg = &segs[i];
        let fail = |error| Error::Segment {
            segment: i,
            mem: self.mem,
            error,
        };
        let (status, pending) = match self.hunk_state(i)? {
            HunkState::Pending => (seg.status, true),
            HunkState::Approx(_) => (SegmentStatus::Approx, false),
            HunkState::Ready(_) => (seg.status, false),
        };

        let ctx = req.context.min(MAX_CONTEXT);
        let skip = match from {
            Some(line) if seg.status == SegmentStatus::Equal => line.0 - seg.a.start.0,
            _ => 0,
        };
        let (first_a, first_b) = (seg.a.start.0 + skip, seg.b.start.0 + skip);
        let pre = ctx.min(first_a).min(first_b);
        let post = ctx
            .min(diff.len_a() - seg.a.end.0)
            .min(diff.len_b() - seg.b.end.0);
        let window_a = first_a - pre..seg.a.end.0 + post;
        let window_b = first_b - pre..seg.b.end.0 + post;

        let cap = pool.capacity(Half::Refine).0;
        // One line on each side and the line buffer must fit an empty
        // pool, so a small budget narrows lines instead of refusing them.
        let line_bytes = req
            .max_line_bytes
            .clamp(1, MAX_LINE_BYTES)
            .min(cap.saturating_sub(2 * FIRST_LINES * LINE + 68) / (2 * TEXT_PER_BYTE + 2))
            .max(1);
        let max_lines = req.max_lines.clamp(1, MAX_LINES);
        let scratch = 2 * (line_bytes + 2) + 64;
        let worst = |n: u64| n.min(max_lines) * (LINE + TEXT_PER_BYTE * line_bytes);
        let want = 2 * worst(window_a.end - window_a.start)
            .max(worst(window_b.end - window_b.start))
            + scratch;
        let least = 2 * (FIRST_LINES * LINE + TEXT_PER_BYTE * line_bytes) + scratch;
        // A short window can want less than the floor every excerpt is
        // checked against.
        let grant = want.max(least).min(pool.available(Half::Refine).0);
        if grant < least {
            return Err(Error::ResponsesBusy);
        }
        let mut held = pool
            .try_reserve(Half::Refine, Bytes(grant))
            .map_err(|_| Error::ResponsesBusy)?;

        let mut buf = LineBuf::new();
        let mut room = (grant - scratch) / 2;
        let opts = (max_lines, line_bytes);
        let mut a = column(diff.lines_a(), window_a, opts, room, &mut buf).map_err(fail)?;
        room = room * 2 - a.1;
        let mut b = column(diff.lines_b(), window_b, opts, room, &mut buf).map_err(fail)?;
        let used = a.1 + b.1 + scratch;
        drop(buf);
        held.shrink(Bytes(grant - used.min(grant)));

        for block in diff.moved_blocks() {
            mark(&mut a.0, &block.a);
            mark(&mut b.0, &block.b);
        }
        Ok(Excerpt {
            segment: i,
            status,
            pending,
            range: seg.clone(),
            a: a.0,
            b: b.0,
            _held: held,
        })
    }
}

/// Sets `moved` on the lines of `col` inside `block`.
fn mark(col: &mut Column, block: &std::ops::Range<LineNo>) {
    let start = col.start.0;
    let end = start + col.lines.len() as u64;
    let (from, to) = (block.start.0.max(start), block.end.0.min(end));
    for n in from..to {
        col.lines[usize::try_from(n - start).expect("index of a line held")].moved = true;
    }
}

/// Reads the lines `window` of `lines` until `max_lines` lines are shown
/// or `room` bytes might not hold the next one. Returns the column and the
/// bytes it holds.
fn column(
    lines: &impl LineAccess,
    window: std::ops::Range<u64>,
    (max_lines, line_bytes): (u64, u64),
    room: u64,
    buf: &mut LineBuf,
) -> rift_core::Result<(Column, u64)> {
    let total = window.end - window.start;
    let limit = usize::try_from(total.min(max_lines)).expect("at most MAX_LINES");
    let mut out: Vec<Line> = Vec::new();
    let mut used = 0;
    for line in window.clone() {
        let grow = if out.len() == out.capacity() {
            (out.capacity() * 2).max(FIRST_LINES_USIZE).min(limit) - out.capacity()
        } else {
            0
        };
        let worst = grow as u64 * LINE + TEXT_PER_BYTE * line_bytes;
        if out.len() == limit || used + worst > room {
            break;
        }
        if grow > 0 {
            out.try_reserve_exact(grow).map_err(|_| oom())?;
            used += grow as u64 * LINE;
        }
        let shown = read_line(lines, LineNo(line), line_bytes, buf)?;
        used += shown.text.capacity() as u64;
        out.push(shown);
    }
    let col = Column {
        start: LineNo(window.start),
        total,
        lines: out,
    };
    Ok((col, used))
}

fn oom() -> rift_core::Error {
    std::io::Error::from(std::io::ErrorKind::OutOfMemory).into()
}

/// One line, cut at `max` bytes of text.
fn read_line(
    lines: &impl LineAccess,
    line: LineNo,
    max: u64,
    buf: &mut LineBuf,
) -> rift_core::Result<Line> {
    // Two bytes over the limit, so a line that fits keeps its terminator.
    let full = lines.read_line_prefix(line, Bytes(max + 2), buf)?.0;
    let raw = buf.line(0);
    let max_len = usize::try_from(max).expect("at most MAX_LINE_BYTES");
    if full == raw.len() as u64 {
        let eol = eol(raw);
        let body = &raw[..raw.len() - eol_len(eol)];
        let truncated = body.len() > max_len;
        let (text, lossy) = decode(&body[..body.len().min(max_len)], truncated);
        return Ok(Line {
            text,
            bytes: body.len() as u64,
            eol,
            truncated,
            lossy,
            moved: false,
        });
    }
    // Longer than the prefix, so cut; the terminator lies past the prefix,
    // in the last two bytes.
    let (text, lossy) = decode(&raw[..max_len], true);
    lines.read_line_part(line, Bytes(full - 2), Bytes(2), buf)?;
    let eol = eol(buf.line(0));
    Ok(Line {
        text,
        bytes: full - eol_len(eol) as u64,
        eol,
        truncated: true,
        lossy,
        moved: false,
    })
}

/// `shown` as text, and whether it was not valid UTF-8. A cut line loses
/// the sequence the cut left incomplete instead of a U+FFFD for it.
fn decode(shown: &[u8], cut: bool) -> (String, bool) {
    let shown = if cut { cut_partial_char(shown) } else { shown };
    match String::from_utf8_lossy(shown) {
        Cow::Borrowed(s) => (s.to_owned(), false),
        Cow::Owned(s) => (s, true),
    }
}

fn eol(end: &[u8]) -> Eol {
    if end.ends_with(b"\r\n") {
        Eol::CrLf
    } else if end.ends_with(b"\n") {
        Eol::Lf
    } else {
        Eol::None
    }
}

fn eol_len(eol: Eol) -> usize {
    match eol {
        Eol::Lf => 1,
        Eol::CrLf => 2,
        Eol::None => 0,
    }
}

/// `b` without a UTF-8 sequence that the cut at its end left incomplete.
fn cut_partial_char(b: &[u8]) -> &[u8] {
    for k in 1..=b.len().min(3) {
        let need = match b[b.len() - k] {
            0x80..=0xBF => continue,
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => return b,
        };
        return if need > k { &b[..b.len() - k] } else { b };
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_incomplete_last_sequence_is_cut() {
        assert_eq!(cut_partial_char(b"ab"), b"ab");
        assert_eq!(cut_partial_char("aé".as_bytes()), "aé".as_bytes());
        assert_eq!(cut_partial_char(&"aé".as_bytes()[..2]), b"a");
        assert_eq!(cut_partial_char(&"a€".as_bytes()[..3]), b"a");
        assert_eq!(cut_partial_char(b"a\xff"), b"a\xff");
    }
}
