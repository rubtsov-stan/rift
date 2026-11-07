//! The terminal interface's drawing of line text, which passes every line
//! through its sanitizer, on a test backend.
//!
//! The target draws whole frames through `App::draw` and checks what
//! lands in the cells, which is what a backend writes to the terminal;
//! `sanitize` checks the sanitizer alone.

use std::ops::Range;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rift_core::{
    DiffView, Edit, Hunk, HunkState, LineNo, MemLines, Progress, Segment, SegmentStatus,
};
use rift_tui::App;

use crate::{fields, split_lines};

/// Two segments: the common leading lines, equal, then the rest of A
/// deleted and the rest of B inserted.
struct View {
    segments: Vec<Segment>,
    hunks: Vec<Arc<Hunk>>,
    approx: bool,
    len: [u64; 2],
}

impl View {
    fn new(a: &[&[u8]], b: &[&[u8]], approx: bool) -> Self {
        let same = a.iter().zip(b).take_while(|(x, y)| x == y).count() as u64;
        let (la, lb) = (a.len() as u64, b.len() as u64);
        let mut segments = Vec::new();
        let mut hunks = Vec::new();
        if same > 0 {
            segments.push(Segment {
                a: LineNo(0)..LineNo(same),
                b: LineNo(0)..LineNo(same),
                status: SegmentStatus::Equal,
            });
            hunks.push(Arc::new(Hunk {
                edits: vec![Edit::Equal {
                    a: LineNo(0),
                    b: LineNo(0),
                    len: same,
                }],
                approx: false,
            }));
        }
        if same < la || same < lb {
            let mut edits = Vec::new();
            if same < la {
                edits.push(Edit::Delete {
                    a: LineNo(same),
                    len: la - same,
                });
            }
            if same < lb {
                edits.push(Edit::Insert {
                    b: LineNo(same),
                    len: lb - same,
                });
            }
            segments.push(Segment {
                a: LineNo(same)..LineNo(la),
                b: LineNo(same)..LineNo(lb),
                status: if approx {
                    SegmentStatus::Approx
                } else {
                    SegmentStatus::Differs
                },
            });
            hunks.push(Arc::new(Hunk { edits, approx }));
        }
        Self {
            segments,
            hunks,
            approx,
            len: [la, lb],
        }
    }
}

impl DiffView for View {
    fn len_a(&self) -> u64 {
        self.len[0]
    }

    fn len_b(&self) -> u64 {
        self.len[1]
    }

    fn segments(&self) -> &[Segment] {
        &self.segments
    }

    fn hunk(&self, segment: usize) -> HunkState {
        let h = Arc::clone(&self.hunks[segment]);
        if self.approx && self.segments[segment].status != SegmentStatus::Equal {
            HunkState::Approx(h)
        } else {
            HunkState::Ready(h)
        }
    }

    fn progress(&self) -> Progress {
        let n = self.segments.len() as u64;
        Progress { done: n, total: n }
    }

    fn set_visible(&self, _range: Range<LineNo>) {}
}

/// The largest screen drawn. The sanitizer and the layout do not change
/// with size past a typical terminal, and bigger screens only cost time
/// that content could use.
const MAX_COLS: u16 = 80;
const MAX_ROWS: u16 = 24;

/// A key from one byte: navigation, go-to-line digits and editing keys.
fn key(byte: u8) -> KeyEvent {
    let code = match byte % 12 {
        0 => KeyCode::Char('n'),
        1 => KeyCode::Char('p'),
        2 => KeyCode::Char(' '),
        3 => KeyCode::Char('b'),
        4 => KeyCode::Char('m'),
        5 => KeyCode::Char('g'),
        6..=9 => KeyCode::Char(char::from(b'0' + byte / 12 % 10)),
        10 => KeyCode::Enter,
        _ => KeyCode::Backspace,
    };
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Characters that must never reach the terminal raw: C0 and C1 controls,
/// bidirectional controls, zero-width and other default-ignorable code
/// points, line and paragraph separators, interlinear annotation marks.
pub(crate) fn hidden(c: char) -> bool {
    c.is_control()
        || matches!(c as u32,
            0xAD | 0x34F | 0x61C | 0x115F..=0x1160 | 0x17B4..=0x17B5 | 0x180B..=0x180F
            | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x206F | 0x3164
            | 0xFE00..=0xFE0F | 0xFEFF | 0xFFA0 | 0xFFF0..=0xFFFB | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A | 0xE0000..=0xE0FFF)
}

/// Draws A against B in a terminal of the size the first two bytes give,
/// capped at [`MAX_COLS`] by [`MAX_ROWS`], after each of up to 32 keys from
/// the third field.
///
/// Fields: width and height bytes, approx flag byte, then A, B and the
/// keys separated by NUL.
///
/// Property: no frame puts a hidden character (see [`hidden`]) in any
/// cell, whatever bytes the lines hold.
///
/// # Panics
///
/// On any property violation.
pub fn check(data: &[u8]) {
    let (w, h, flags, rest) = match data {
        [w, h, f, rest @ ..] => (*w, *h, *f, rest),
        _ => return,
    };
    let [a, b, keys] = fields(rest);
    if a.is_empty() && b.is_empty() {
        return;
    }
    let (lines_a, lines_b) = (split_lines(a), split_lines(b));
    let view = View::new(&lines_a, &lines_b, flags & 1 != 0);
    let (ma, mb) = (MemLines::new(a), MemLines::new(b));
    let mut app = App::new(&view, &ma, &mb);
    let backend = TestBackend::new(
        u16::from(w).clamp(1, MAX_COLS),
        u16::from(h).clamp(1, MAX_ROWS),
    );
    let mut term = Terminal::new(backend).expect("test backend");
    for k in std::iter::once(None).chain(keys.iter().take(32).map(Some)) {
        if let Some(&k) = k {
            app.handle_key(key(k));
        }
        term.draw(|f| app.draw(f)).expect("draw");
        for (i, cell) in term.backend().buffer().content().iter().enumerate() {
            if let Some(c) = cell.symbol().chars().find(|&c| hidden(c)) {
                panic!("cell {i} holds U+{:04X}", c as u32);
            }
        }
    }
}
