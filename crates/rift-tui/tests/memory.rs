//! The interface keeps nothing per segment: its heap use at a million
//! segments is the same small constant as at a thousand.
#![allow(missing_docs)]

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rift_core::{
    Bytes, DiffView, Edit, Hunk, HunkState, LineAccess, LineBuf, LineNo, Progress, Result, Segment,
    SegmentStatus,
};
use rift_tui::App;

#[path = "common/counting.rs"]
mod counting;

use counting::{CURRENT, PEAK};

#[global_allocator]
static A: counting::Counting = counting::Counting;

/// Equal(2) and Differs(1, 1) alternating; every hunk is the same shared
/// script (line numbers on screen are wrong, which this test ignores).
struct Big {
    segments: Vec<Segment>,
    hunk: Arc<Hunk>,
    len: u64,
}

impl Big {
    fn new(n: usize) -> Self {
        let (mut at, mut segments) = (0u64, Vec::with_capacity(n));
        for i in 0..n {
            let (len, status) = if i % 2 == 0 {
                (2, SegmentStatus::Equal)
            } else {
                (1, SegmentStatus::Differs)
            };
            segments.push(Segment {
                a: LineNo(at)..LineNo(at + len),
                b: LineNo(at)..LineNo(at + len),
                status,
            });
            at += len;
        }
        let edits = vec![
            Edit::Delete {
                a: LineNo(0),
                len: 1,
            },
            Edit::Insert {
                b: LineNo(0),
                len: 1,
            },
        ];
        Self {
            segments,
            hunk: Arc::new(Hunk {
                edits,
                approx: false,
            }),
            len: at,
        }
    }
}

impl DiffView for Big {
    fn len_a(&self) -> u64 {
        self.len
    }
    fn len_b(&self) -> u64 {
        self.len
    }
    fn segments(&self) -> &[Segment] {
        &self.segments
    }
    fn hunk(&self, _: usize) -> HunkState {
        HunkState::Ready(Arc::clone(&self.hunk))
    }
    fn progress(&self) -> Progress {
        Progress { done: 1, total: 1 }
    }
    fn set_visible(&self, _: Range<LineNo>) {}
}

struct Text(u64);

impl LineAccess for Text {
    fn line_count(&self) -> u64 {
        self.0
    }
    fn read_lines(&self, range: Range<LineNo>, buf: &mut LineBuf) -> Result<()> {
        buf.clear();
        for _ in range.start.0..range.end.0 {
            buf.push(b"some line of text\n")?;
        }
        Ok(())
    }
    fn read_line_prefix(&self, _: LineNo, max: Bytes, buf: &mut LineBuf) -> Result<Bytes> {
        let line = b"some line of text\n";
        buf.clear();
        buf.push(&line[..line.len().min(usize::try_from(max.0).unwrap())])?;
        Ok(Bytes(line.len() as u64))
    }
}

/// Peak heap the interface adds while a few hundred keys are handled.
fn interface_peak(segments: usize) -> usize {
    let view = Big::new(segments);
    let text = Text(view.len);
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let base = CURRENT.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let mut app = App::new(&view, &text, &text);
    let keys = "n n p b g500000\n g1\n n n".chars().map(|c| {
        if c == '\n' {
            KeyCode::Enter
        } else {
            KeyCode::Char(c)
        }
    });
    let keys: Vec<_> = keys.collect();
    for k in keys.iter().cycle().take(300) {
        app.handle_key(KeyEvent::new(*k, KeyModifiers::NONE));
        app.draw(&mut term.get_frame());
    }
    PEAK.load(Ordering::Relaxed) - base
}

#[test]
fn heap_does_not_grow_with_segments() {
    let small = interface_peak(1_000);
    let big = interface_peak(1_000_000);
    println!("interface heap peak: {small} B at 1k segments, {big} B at 1M segments");
    assert!(big < 64 << 10, "{big} bytes");
    assert!(big <= small + 1024, "{small} -> {big}");
}
