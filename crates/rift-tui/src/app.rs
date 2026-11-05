//! Interface state and navigation. Drawing lives in `render`.

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rift_core::{DiffView, HunkState, LineAccess, LineBuf, LineNo, Progress, SegmentStatus};

use crate::minimap::{self, Minimap};

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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Pos {
    pub seg: usize,
    pub off: u64,
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
    pub status: String,
    pub minimap: Minimap,
}

/// The interface over one diff.
pub struct App<'a> {
    pub(crate) view: &'a dyn DiffView,
    pub(crate) a: &'a dyn LineAccess,
    pub(crate) b: &'a dyn LineAccess,
    pub(crate) pos: Pos,
    pub(crate) mode: Mode,
    pub(crate) message: &'static str,
    /// Indices of segments that are not Equal, ascending.
    pub(crate) diffs: Vec<usize>,
    /// Minimap-space start of every segment, plus the total.
    pub(crate) starts: Vec<u64>,
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
        let diffs = (0..segs.len())
            .filter(|&i| segs[i].status != SegmentStatus::Equal)
            .collect();
        Self {
            view,
            a,
            b,
            pos: Pos::default(),
            mode: Mode::Normal,
            message: "",
            diffs,
            starts: minimap::starts(segs),
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
                        self.goto_line(line);
                    }
                }
                KeyCode::Esc => self.mode = Mode::Normal,
                _ => {}
            }
            return Action::Continue;
        }
        let page = u64::from(self.rows.max(2) - 1);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => return Action::Quit,
            KeyCode::Char('q') | KeyCode::Esc => return Action::Quit,
            KeyCode::Char('n') => self.next_diff(),
            KeyCode::Char('p' | 'N') => self.prev_diff(),
            KeyCode::PageDown | KeyCode::Char(' ') => self.scroll_down(page),
            KeyCode::PageUp | KeyCode::Char('b') => self.scroll_up(page),
            KeyCode::Char('g' | ':') => self.mode = Mode::Goto(None),
            _ => {}
        }
        Action::Continue
    }

    fn next_diff(&mut self) {
        let i = self.diffs.partition_point(|&d| d <= self.pos.seg);
        match self.diffs.get(i) {
            Some(&seg) => self.pos = Pos { seg, off: 0 },
            None => self.message = "no next difference",
        }
    }

    fn prev_diff(&mut self) {
        let inside = self.diffs.binary_search(&self.pos.seg).is_ok();
        if inside && self.pos.off > 0 {
            self.pos.off = 0;
            return;
        }
        let i = self.diffs.partition_point(|&d| d < self.pos.seg);
        match i.checked_sub(1) {
            Some(i) => {
                self.pos = Pos {
                    seg: self.diffs[i],
                    off: 0,
                }
            }
            None => self.message = "no previous difference",
        }
    }

    /// Rows a segment occupies. A pending hunk is one placeholder row.
    /// O(edits) for a refined hunk.
    pub(crate) fn rows_of(&self, seg: usize) -> u64 {
        let s = &self.view.segments()[seg];
        if s.status == SegmentStatus::Equal {
            return s.len_a().max(1);
        }
        match self.view.hunk(seg) {
            HunkState::Pending => 1,
            HunkState::Ready(h) | HunkState::Approx(h) => {
                let n: u64 = h
                    .edits
                    .iter()
                    .map(|e| match *e {
                        rift_core::Edit::Equal { len, .. }
                        | rift_core::Edit::Delete { len, .. }
                        | rift_core::Edit::Insert { len, .. } => len,
                    })
                    .sum();
                n.max(1)
            }
        }
    }

    pub(crate) fn scroll_down(&mut self, mut n: u64) {
        let last = self.view.segments().len().saturating_sub(1);
        loop {
            let rows = self.rows_of(self.pos.seg);
            if self.pos.off + n < rows {
                self.pos.off += n;
                return;
            }
            n -= rows - self.pos.off;
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
        } else if let HunkState::Ready(h) | HunkState::Approx(h) = self.view.hunk(seg) {
            let mut rows = 0;
            let mut found = None;
            for e in &h.edits {
                match *e {
                    rift_core::Edit::Equal { a, len, .. } | rift_core::Edit::Delete { a, len } => {
                        if (a.0..a.0 + len).contains(&want) {
                            found = Some(rows + (want - a.0));
                            break;
                        }
                        rows += len;
                    }
                    rift_core::Edit::Insert { len, .. } => rows += len,
                }
            }
            found.unwrap_or(0)
        } else {
            0
        };
        self.pos = Pos { seg, off };
    }
}
