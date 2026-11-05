//! Interface state and navigation. Drawing lives in `render`.

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rift_core::{DiffView, HunkState, LineAccess, LineBuf, LineNo, Progress, SegmentStatus};

use crate::cache::LineCache;
use crate::minimap::{self, Cursor, Minimap};

/// What the loop should do after a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Keep running.
    Continue,
    /// Leave the interface.
    Quit,
}

/// Top row of the screen: segment index and row within that segment.
///
/// A row is one line of an Equal segment, one edit line of a refined hunk, or
/// the single placeholder row of a pending hunk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Pos {
    pub seg: usize,
    pub off: u64,
}

/// The line the top row should show inside a changed segment, kept across
/// changes of the hunk's state: a pending hunk is one row and a refined
/// one many, so a row offset alone would point elsewhere once it changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Anchor {
    /// A line of A, 0-based: an equal or deleted row.
    A(u64),
    /// A line of B, 0-based: an inserted row.
    B(u64),
    /// The last row: the top was reached by scrolling up from below.
    Last,
}

#[derive(Debug)]
pub(crate) enum Mode {
    Normal,
    /// Digits typed so far; `None` before the first digit.
    Goto(Option<u64>),
}

/// Buffers reused by every frame so drawing allocates nothing after warm-up.
#[derive(Debug, Default)]
pub(crate) struct Scratch {
    pub lines: LineBuf,
    pub text: String,
    pub prefix: String,
    pub cache: LineCache,
    pub status: String,
    pub minimap: Minimap,
}

/// The interface over one diff.
pub struct App<'a> {
    pub(crate) view: &'a dyn DiffView,
    pub(crate) a: &'a dyn LineAccess,
    pub(crate) b: &'a dyn LineAccess,
    pub(crate) pos: Pos,
    /// What `pos.off` stands for when `pos.seg` is a changed segment.
    pub(crate) anchor: Option<Anchor>,
    pub(crate) mode: Mode,
    pub(crate) message: &'static str,
    /// Segments that are not Equal, and the minimap weight of all segments.
    /// One pass at construction; nothing is kept per segment.
    pub(crate) diff_total: u64,
    pub(crate) weight_total: u64,
    /// Differences before the top segment, and the weight before it.
    pub(crate) seen: Cursor,
    pub(crate) before: Cursor,
    pub(crate) rows: u16,
    pub(crate) reported: Option<Range<LineNo>>,
    pub(crate) drawn_progress: Option<Progress>,
    pub(crate) pending_shown: bool,
    pub(crate) scratch: Scratch,
}

impl<'a> App<'a> {
    /// Builds the interface; `a` and `b` give the text of the two files.
    #[must_use]
    pub fn new(view: &'a dyn DiffView, a: &'a dyn LineAccess, b: &'a dyn LineAccess) -> Self {
        let segs = view.segments();
        let diff_total = segs.iter().map(minimap::is_diff).sum();
        let weight_total = segs.iter().map(minimap::weight).sum();
        Self {
            view,
            a,
            b,
            pos: Pos::default(),
            anchor: None,
            mode: Mode::Normal,
            message: "",
            diff_total,
            weight_total,
            seen: Cursor::default(),
            before: Cursor::default(),
            rows: 20,
            reported: None,
            drawn_progress: None,
            pending_shown: false,
            scratch: Scratch::default(),
        }
    }

    /// Whether a tick should redraw: refinement moved or a pending hunk is
    /// on screen.
    #[must_use]
    pub fn needs_redraw(&self) -> bool {
        self.pending_shown || self.drawn_progress != Some(self.view.progress())
    }

    /// Applies one key press.
    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        self.message = "";
        let held = key.modifiers;
        if key.code == KeyCode::Char('c') && held.contains(KeyModifiers::CONTROL) {
            return Action::Quit;
        }
        // Ctrl-C is the only shortcut; Ctrl-N is not `n`.
        if matches!(key.code, KeyCode::Char(_))
            && held.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Action::Continue;
        }
        if let Mode::Goto(n) = self.mode {
            match key.code {
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    let d = u64::from(c as u8 - b'0');
                    self.mode =
                        Mode::Goto(Some(n.unwrap_or(0).saturating_mul(10).saturating_add(d)));
                }
                KeyCode::Backspace => self.mode = Mode::Goto(n.map(|v| v / 10)),
                KeyCode::Enter => {
                    self.mode = Mode::Normal;
                    if let Some(line) = n {
                        self.anchor = None;
                        self.goto_line(line);
                    }
                }
                KeyCode::Esc => self.mode = Mode::Normal,
                _ => {}
            }
            return Action::Continue;
        }
        let page = u64::from(self.rows.max(2) - 1);
        if matches!(
            key.code,
            KeyCode::Char('n' | 'p' | 'N' | ' ' | 'b') | KeyCode::PageDown | KeyCode::PageUp
        ) {
            self.anchor = None;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Action::Quit,
            KeyCode::Char('n') => self.next_diff(),
            KeyCode::Char('p' | 'N') => self.prev_diff(),
            KeyCode::PageDown | KeyCode::Char(' ') => self.page_down(page),
            KeyCode::PageUp | KeyCode::Char('b') => {
                let from = self.pos.seg;
                self.scroll_up(page);
                if self.pos.seg < from && self.pending(self.pos.seg) {
                    self.anchor = Some(Anchor::Last);
                }
            }
            KeyCode::Char('g' | ':') => self.mode = Mode::Goto(None),
            _ => {}
        }
        Action::Continue
    }

    fn next_diff(&mut self) {
        let segs = self.view.segments();
        let next = (self.pos.seg + 1..segs.len()).find(|&i| segs[i].status != SegmentStatus::Equal);
        match next {
            Some(seg) => self.pos = Pos { seg, off: 0 },
            None => self.message = "no next difference",
        }
    }

    fn prev_diff(&mut self) {
        let segs = self.view.segments();
        let inside = segs
            .get(self.pos.seg)
            .is_some_and(|s| s.status != SegmentStatus::Equal);
        if inside && self.pos.off > 0 {
            self.pos.off = 0;
            return;
        }
        let prev = (0..self.pos.seg)
            .rev()
            .find(|&i| segs[i].status != SegmentStatus::Equal);
        match prev {
            Some(seg) => self.pos = Pos { seg, off: 0 },
            None => self.message = "no previous difference",
        }
    }

    /// Whether `seg` is a changed segment whose hunk is not ready.
    fn pending(&self, seg: usize) -> bool {
        self.view.segments()[seg].status != SegmentStatus::Equal
            && matches!(self.view.hunk(seg), HunkState::Pending)
    }

    /// Rows a segment occupies. O(edits) for a refined hunk.
    pub(crate) fn rows_of(&self, seg: usize) -> u64 {
        let s = &self.view.segments()[seg];
        let state = (s.status != SegmentStatus::Equal).then(|| self.view.hunk(seg));
        rows_in(s, state.as_ref())
    }

    /// Scrolls down but stops with the last row at the bottom of the view.
    /// Never moves up: a jump to a difference near the end can sit below the
    /// last full page.
    fn page_down(&mut self, n: u64) {
        let old = self.pos;
        self.scroll_down(n);
        let max = self.max_top();
        if self.pos > max {
            self.pos = old.max(max);
        }
    }

    /// The highest top row that still fills the view down to the last row.
    fn max_top(&mut self) -> Pos {
        let Some(last) = self.view.segments().len().checked_sub(1) else {
            return self.pos;
        };
        let saved = self.pos;
        self.pos = Pos {
            seg: last,
            off: self.rows_of(last) - 1,
        };
        self.scroll_up(u64::from(self.rows.max(1) - 1));
        std::mem::replace(&mut self.pos, saved)
    }

    fn scroll_down(&mut self, mut n: u64) {
        let last = self.view.segments().len().saturating_sub(1);
        loop {
            let rows = self.rows_of(self.pos.seg);
            // A hunk evicted since the last frame has shrunk to one row.
            self.pos.off = self.pos.off.min(rows - 1);
            if self.pos.off.saturating_add(n) < rows {
                self.pos.off += n;
                return;
            }
            n = n.saturating_sub(rows - self.pos.off);
            if self.pos.seg >= last {
                self.pos.off = rows - 1;
                return;
            }
            self.pos = Pos {
                seg: self.pos.seg + 1,
                off: 0,
            };
        }
    }

    pub(crate) fn scroll_up(&mut self, mut n: u64) {
        loop {
            if n <= self.pos.off {
                self.pos.off -= n;
                return;
            }
            n -= self.pos.off;
            if self.pos.seg == 0 {
                self.pos.off = 0;
                return;
            }
            self.pos.seg -= 1;
            self.pos.off = self.rows_of(self.pos.seg);
        }
    }

    /// Jumps to 1-based A line `line`, clamped to the file.
    fn goto_line(&mut self, line: u64) {
        let segs = self.view.segments();
        if segs.is_empty() {
            return;
        }
        let want = line
            .saturating_sub(1)
            .min(self.view.len_a().saturating_sub(1));
        let seg = segs
            .partition_point(|s| s.a.end.0 <= want)
            .min(segs.len() - 1);
        let s = &segs[seg];
        let off = if s.status == SegmentStatus::Equal {
            want.saturating_sub(s.a.start.0)
        } else {
            // Resolved by the next frame, now or once the hunk is ready.
            self.anchor = Some(Anchor::A(want));
            0
        };
        self.pos = Pos { seg, off };
    }
}

/// The row of a refined hunk that shows `anchor`, if any.
pub(crate) fn row_of(h: &rift_core::Hunk, anchor: Anchor) -> Option<u64> {
    let mut rows = 0;
    for e in &h.edits {
        let (side_a, start, len) = match *e {
            rift_core::Edit::Equal { a, len, .. } | rift_core::Edit::Delete { a, len } => {
                (true, a.0, len)
            }
            rift_core::Edit::Insert { b, len } => (false, b.0, len),
        };
        let line = match anchor {
            Anchor::A(l) if side_a => Some(l),
            Anchor::B(l) if !side_a => Some(l),
            _ => None,
        };
        if let Some(l) = line
            && (start..start + len).contains(&l)
        {
            return Some(rows + (l - start));
        }
        rows += len;
    }
    (anchor == Anchor::Last).then(|| rows.saturating_sub(1))
}

/// What row `off` of a refined hunk shows.
pub(crate) fn anchor_at(h: &rift_core::Hunk, off: u64) -> Anchor {
    let mut rows = 0;
    for e in &h.edits {
        let (len, anchor) = match *e {
            rift_core::Edit::Equal { a, len, .. } | rift_core::Edit::Delete { a, len } => {
                (len, Anchor::A(a.0 + off.saturating_sub(rows)))
            }
            rift_core::Edit::Insert { b, len } => (len, Anchor::B(b.0 + off.saturating_sub(rows))),
        };
        if off < rows + len {
            return anchor;
        }
        rows += len;
    }
    Anchor::Last
}

/// Rows of a segment given its hunk state (`None` for an Equal segment). A
/// pending hunk is one placeholder row.
pub(crate) fn rows_in(seg: &rift_core::Segment, state: Option<&HunkState>) -> u64 {
    match state {
        None => seg.len_a().max(1),
        Some(HunkState::Pending) => 1,
        Some(HunkState::Ready(h) | HunkState::Approx(h)) => h
            .edits
            .iter()
            .map(|e| match *e {
                rift_core::Edit::Equal { len, .. }
                | rift_core::Edit::Delete { len, .. }
                | rift_core::Edit::Insert { len, .. } => len,
            })
            .sum::<u64>()
            .max(1),
    }
}
