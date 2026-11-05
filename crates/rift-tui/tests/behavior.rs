//! Keys, visible-range reporting and the event loop, on the mock view.
#![allow(missing_docs)]

mod common;

use std::collections::VecDeque;
use std::convert::Infallible;

use common::{Mock, Spec};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rift_core::{Bytes, DiffView, LineAccess, LineBuf, LineNo, MemLines, Result};
use rift_tui::{Action, App, run};

fn key(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

fn chars(app: &mut App<'_>, keys: &str) {
    for c in keys.chars() {
        let code = if c == '\n' {
            KeyCode::Enter
        } else {
            KeyCode::Char(c)
        };
        app.handle_key(key(code));
    }
}

/// 100 lines in A: equal 0..40, changed 40..45, equal 45..100.
fn mock() -> Mock {
    Mock::build(
        &[
            Spec::Equal(40),
            Spec::Diff {
                del: 5,
                mid: 0,
                ins: 3,
            },
            Spec::Equal(55),
        ],
        true,
    )
}

fn draw(app: &mut App<'_>, term: &mut Terminal<TestBackend>) {
    term.draw(|f| app.draw(f)).unwrap();
}

fn last_visible(m: &Mock) -> std::ops::Range<u64> {
    let r = m.view.visible.lock().unwrap().last().cloned().unwrap();
    r.start.0..r.end.0
}

#[test]
fn quit_keys() {
    let m = mock();
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    assert_eq!(app.handle_key(key(KeyCode::Char('q'))), Action::Quit);
    assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::Quit);
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.handle_key(ctrl_c), Action::Quit);
    assert_eq!(app.handle_key(key(KeyCode::Char('c'))), Action::Continue);
}

#[test]
fn next_and_previous_difference_walk_the_segments() {
    let m = mock();
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
    draw(&mut app, &mut term);
    assert_eq!(last_visible(&m).start, 0);
    chars(&mut app, "n");
    draw(&mut app, &mut term);
    assert_eq!(last_visible(&m).start, 40);
    chars(&mut app, "n");
    draw(&mut app, &mut term);
    let text = format!("{:?}", term.backend().buffer());
    assert!(text.contains("no next difference"));
    chars(&mut app, "p");
    draw(&mut app, &mut term);
    assert_eq!(last_visible(&m).start, 40);
    chars(&mut app, "p");
    draw(&mut app, &mut term);
    let text = format!("{:?}", term.backend().buffer());
    assert!(text.contains("no previous difference"));
}

#[test]
fn paging_moves_by_a_screen_and_stops_at_the_ends() {
    let m = mock();
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
    draw(&mut app, &mut term);
    // 14 rows minus 3 minimap rows and the status line: 10 rows, page is 9.
    let first = last_visible(&m);
    assert_eq!(first, 0..10);
    app.handle_key(key(KeyCode::PageDown));
    draw(&mut app, &mut term);
    assert_eq!(last_visible(&m).start, 9);
    app.handle_key(key(KeyCode::PageUp));
    app.handle_key(key(KeyCode::PageUp));
    draw(&mut app, &mut term);
    assert_eq!(last_visible(&m).start, 0);
    for _ in 0..50 {
        app.handle_key(key(KeyCode::PageDown));
    }
    draw(&mut app, &mut term);
    // The last line stops at the bottom: the final 10 rows stay on screen.
    assert_eq!(last_visible(&m), 90..100);
}

#[test]
fn go_to_line_clamps_cancels_and_edits() {
    let m = mock();
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
    chars(&mut app, "g50\n");
    draw(&mut app, &mut term);
    assert_eq!(last_visible(&m).start, 49);
    chars(&mut app, "g7\u{0}");
    app.handle_key(key(KeyCode::Backspace));
    chars(&mut app, "3\n");
    draw(&mut app, &mut term);
    assert_eq!(last_visible(&m).start, 2);
    chars(&mut app, "g9");
    app.handle_key(key(KeyCode::Esc));
    draw(&mut app, &mut term);
    assert_eq!(last_visible(&m).start, 2);
    chars(&mut app, "g99999999999999999999999\n");
    draw(&mut app, &mut term);
    assert_eq!(last_visible(&m).start, 99);
    chars(&mut app, "g0\n");
    draw(&mut app, &mut term);
    assert_eq!(last_visible(&m).start, 0);
}

#[test]
fn go_to_a_line_inside_a_hunk_lands_on_its_row() {
    let m = mock();
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
    chars(&mut app, "g43\n");
    draw(&mut app, &mut term);
    assert_eq!(last_visible(&m).start, 42);
}

#[test]
fn visible_range_is_reported_once_per_change() {
    let m = mock();
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
    draw(&mut app, &mut term);
    draw(&mut app, &mut term);
    assert_eq!(m.view.visible.lock().unwrap().len(), 1);
    app.handle_key(key(KeyCode::PageDown));
    draw(&mut app, &mut term);
    assert_eq!(m.view.visible.lock().unwrap().len(), 2);
}

#[test]
fn inserted_lines_count_with_the_a_line_they_follow() {
    let m = Mock::build(
        &[
            Spec::Equal(2),
            Spec::Diff {
                del: 0,
                mid: 0,
                ins: 2,
            },
            Spec::Equal(2),
        ],
        true,
    );
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    // Two text rows, both inserted lines.
    let mut term = Terminal::new(TestBackend::new(60, 3)).unwrap();
    chars(&mut app, "n");
    draw(&mut app, &mut term);
    assert_eq!(last_visible(&m), 1..2);
}

fn visible_for(specs: &[Spec], keys: &str) -> std::ops::Range<u64> {
    let m = Mock::build(specs, true);
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(60, 3)).unwrap();
    chars(&mut app, keys);
    draw(&mut app, &mut term);
    last_visible(&m)
}

#[test]
fn inserts_at_the_end_of_a_report_the_last_a_line() {
    let ins = Spec::Diff {
        del: 0,
        mid: 0,
        ins: 2,
    };
    assert_eq!(visible_for(&[Spec::Equal(2), ins], "n"), 1..2);
}

#[test]
fn inserts_at_the_top_report_the_first_a_line() {
    let ins = Spec::Diff {
        del: 0,
        mid: 0,
        ins: 2,
    };
    assert_eq!(visible_for(&[ins, Spec::Equal(2)], ""), 0..1);
}

#[test]
fn ctrl_c_quits_from_the_go_to_prompt() {
    let m = mock();
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    chars(&mut app, "g4");
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(app.handle_key(ctrl_c), Action::Quit);
}

#[test]
fn each_hunk_is_fetched_once_per_frame() {
    let m = mock();
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(60, 30)).unwrap();
    chars(&mut app, "n");
    m.view.log_hunk_calls();
    draw(&mut app, &mut term);
    let mut calls = m.view.hunk_calls();
    let total = calls.len();
    calls.sort_unstable();
    calls.dedup();
    assert!(total > 0);
    assert_eq!(calls.len(), total, "a segment was fetched twice");
}

#[test]
fn event_loop_records_one_sample_per_key_and_redraws_on_progress() {
    let m = Mock::build(
        &[
            Spec::Equal(2),
            Spec::Diff {
                del: 1,
                mid: 0,
                ins: 1,
            },
            Spec::Equal(2),
        ],
        false,
    );
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
    let mut events: VecDeque<Option<Event>> = [
        Some(Event::Key(key(KeyCode::Char('n')))),
        None,
        Some(Event::Key(key(KeyCode::PageDown))),
        Some(Event::Key(key(KeyCode::Char('q')))),
    ]
    .into();
    let mut samples = Vec::new();
    let mut record = |d| samples.push(d);
    let mut ticks = 0;
    run(
        &mut term,
        &mut app,
        |_| -> Result<_, Infallible> {
            let e = events.pop_front().expect("loop stops at q");
            if e.is_none() {
                ticks += 1;
                // A pending hunk is on screen: this tick must redraw it.
                assert!(m.view.refine_next());
            }
            Ok(e)
        },
        Some(&mut record),
    )
    .unwrap();
    assert_eq!(ticks, 1);
    assert_eq!(
        samples.len(),
        2,
        "q quits before drawing and records nothing"
    );
}

/// Counts `read_line_prefix` calls per line.
struct Counted<'a> {
    inner: MemLines<'a>,
    prefix_reads: std::sync::Mutex<Vec<u64>>,
}

impl LineAccess for Counted<'_> {
    fn line_count(&self) -> u64 {
        self.inner.line_count()
    }
    fn read_lines(&self, range: std::ops::Range<LineNo>, buf: &mut LineBuf) -> Result<()> {
        self.inner.read_lines(range, buf)
    }
    fn read_line_prefix(&self, line: LineNo, max: Bytes, buf: &mut LineBuf) -> Result<Bytes> {
        self.prefix_reads.lock().unwrap().push(line.0);
        self.inner.read_line_prefix(line, max, buf)
    }

    fn read_line_part(&self, l: LineNo, from: Bytes, max: Bytes, b: &mut LineBuf) -> Result<bool> {
        self.inner.read_line_part(l, from, max, b)
    }
}

#[test]
fn a_huge_line_is_read_once_while_the_window_stays() {
    let mut data = b"first\n".to_vec();
    data.extend(std::iter::repeat_n(b'x', 1 << 20));
    data.push(b'\n');
    for i in 0..40 {
        data.extend_from_slice(format!("line {i}\n").as_bytes());
    }
    let lines = Counted {
        inner: MemLines::new(&data),
        prefix_reads: std::sync::Mutex::new(Vec::new()),
    };
    let m = Mock::build(&[Spec::Equal(lines.line_count())], true);
    let mut app = App::new(&m.view, &lines, &lines);
    let mut term = Terminal::new(TestBackend::new(70, 8)).unwrap();
    let reads_of_huge = |lines: &Counted<'_>| {
        lines
            .prefix_reads
            .lock()
            .unwrap()
            .iter()
            .filter(|&&l| l == 1)
            .count()
    };
    for _ in 0..100 {
        draw(&mut app, &mut term);
    }
    assert_eq!(reads_of_huge(&lines), 1, "redraws must reuse the cache");
    draw(&mut app, &mut term);
    assert_eq!(reads_of_huge(&lines), 1);

    app.handle_key(key(KeyCode::PageDown));
    draw(&mut app, &mut term);
    assert_eq!(reads_of_huge(&lines), 1, "off screen, not read");
    app.handle_key(key(KeyCode::PageUp));
    draw(&mut app, &mut term);
    assert_eq!(reads_of_huge(&lines), 2, "coming back reads it again");
    draw(&mut app, &mut term);
    assert_eq!(reads_of_huge(&lines), 2);

    // A wider terminal needs a longer prefix.
    let mut wide = Terminal::new(TestBackend::new(140, 8)).unwrap();
    draw(&mut app, &mut wide);
    assert_eq!(reads_of_huge(&lines), 3);
}

/// Reports 1/3 on its first progress query and 3/3 afterwards, as if
/// refinement finished between the status bar and the next tick.
struct Racing<'a> {
    inner: &'a common::MockView,
    asked: std::sync::atomic::AtomicUsize,
}

impl DiffView for Racing<'_> {
    fn len_a(&self) -> u64 {
        self.inner.len_a()
    }
    fn len_b(&self) -> u64 {
        self.inner.len_b()
    }
    fn segments(&self) -> &[rift_core::Segment] {
        self.inner.segments()
    }
    fn hunk(&self, segment: usize) -> rift_core::HunkState {
        self.inner.hunk(segment)
    }
    fn progress(&self) -> rift_core::Progress {
        let first = self
            .asked
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            == 0;
        rift_core::Progress {
            done: if first { 1 } else { 3 },
            total: 3,
        }
    }
    fn set_visible(&self, range: std::ops::Range<LineNo>) {
        self.inner.set_visible(range);
    }
}

#[test]
fn a_bar_that_is_out_of_date_is_redrawn_on_the_next_tick() {
    let m = mock();
    let view = Racing {
        inner: &m.view,
        asked: std::sync::atomic::AtomicUsize::new(0),
    };
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(70, 12)).unwrap();
    draw(&mut app, &mut term);
    let shown = format!("{:?}", term.backend().buffer());
    assert!(shown.contains("refining 1/3"), "{shown}");
    assert!(app.needs_redraw(), "the bar shows 1/3 but 3/3 is current");
    draw(&mut app, &mut term);
    assert!(format!("{:?}", term.backend().buffer()).contains("refined 3/3"));
    assert!(!app.needs_redraw());
}

#[test]
fn paging_after_the_hunk_under_the_cursor_is_evicted_does_not_panic() {
    let m = Mock::build(
        &[
            Spec::Equal(2),
            Spec::Diff {
                del: 8,
                mid: 0,
                ins: 8,
            },
            Spec::Equal(40),
        ],
        true,
    );
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
    // Top row is deep inside the 16-row hunk.
    chars(&mut app, "g8\n");
    draw(&mut app, &mut term);
    m.view.evict(1);
    // The hunk is now one placeholder row; the key arrives before the frame.
    app.handle_key(key(KeyCode::PageDown));
    app.handle_key(key(KeyCode::PageUp));
    draw(&mut app, &mut term);
}

#[test]
fn ctrl_and_alt_letters_do_not_trigger_plain_actions() {
    let m = mock();
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
    for mods in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
        for c in ['q', 'n', 'p', 'b', 'g', ' '] {
            let k = KeyEvent::new(KeyCode::Char(c), mods);
            assert_eq!(app.handle_key(k), Action::Continue, "{mods:?}-{c}");
        }
    }
    draw(&mut app, &mut term);
    assert_eq!(last_visible(&m).start, 0, "nothing moved");
    // Not in the go-to prompt either: a modified digit is no digit.
    chars(&mut app, "g");
    let ctrl_5 = KeyEvent::new(KeyCode::Char('5'), KeyModifiers::CONTROL);
    app.handle_key(ctrl_5);
    chars(&mut app, "\n");
    draw(&mut app, &mut term);
    assert_eq!(last_visible(&m).start, 0);
    // Shift is part of typing a capital.
    let shift_n = KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT);
    chars(&mut app, "n");
    app.handle_key(shift_n);
    draw(&mut app, &mut term);
    // Moved to the first difference, and Shift-N found nothing before it.
    assert_eq!(last_visible(&m).start, 40);
}

/// Line 1 always fails to read; counts the attempts.
struct Flaky<'a> {
    inner: MemLines<'a>,
    attempts: std::sync::atomic::AtomicUsize,
}

impl LineAccess for Flaky<'_> {
    fn line_count(&self) -> u64 {
        self.inner.line_count()
    }
    fn read_lines(&self, range: std::ops::Range<LineNo>, buf: &mut LineBuf) -> Result<()> {
        self.inner.read_lines(range, buf)
    }
    fn read_line_prefix(&self, line: LineNo, max: Bytes, buf: &mut LineBuf) -> Result<Bytes> {
        if line.0 == 1 {
            self.attempts
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return Err(std::io::Error::other("disk on fire").into());
        }
        self.inner.read_line_prefix(line, max, buf)
    }

    fn read_line_part(&self, l: LineNo, from: Bytes, max: Bytes, b: &mut LineBuf) -> Result<bool> {
        self.inner.read_line_part(l, from, max, b)
    }
}

#[test]
fn a_failed_read_is_kept_for_the_window_and_retried_when_it_moves() {
    let mut data = Vec::new();
    for i in 0..40 {
        data.extend_from_slice(format!("line {i}\n").as_bytes());
    }
    let lines = Flaky {
        inner: MemLines::new(&data),
        attempts: std::sync::atomic::AtomicUsize::new(0),
    };
    let m = Mock::build(&[Spec::Equal(40)], true);
    let mut app = App::new(&m.view, &lines, &lines);
    let mut term = Terminal::new(TestBackend::new(70, 8)).unwrap();
    let attempts = || lines.attempts.load(std::sync::atomic::Ordering::Relaxed);
    for _ in 0..50 {
        draw(&mut app, &mut term);
    }
    assert_eq!(attempts(), 1);
    assert!(format!("{:?}", term.backend().buffer()).contains("disk on fire"));
    app.handle_key(key(KeyCode::PageDown));
    draw(&mut app, &mut term);
    app.handle_key(key(KeyCode::PageUp));
    draw(&mut app, &mut term);
    draw(&mut app, &mut term);
    assert_eq!(attempts(), 2, "retried once after the window moved away");
}

/// The screen as text, one string per row.
fn rows_of(term: &Terminal<TestBackend>) -> Vec<String> {
    let buf = term.backend().buffer();
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
        .collect()
}

/// Equal 0..40, a changed segment of A lines 40..55 (5 deleted, 10
/// shared, 3 inserted) that starts pending, equal 55..110.
fn pending_mock() -> Mock {
    Mock::build(
        &[
            Spec::Equal(40),
            Spec::Diff {
                del: 5,
                mid: 10,
                ins: 3,
            },
            Spec::Equal(55),
        ],
        false,
    )
}

#[test]
fn rows_below_a_pending_hunk_stay_put_when_it_is_refined() {
    let m = pending_mock();
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
    draw(&mut app, &mut term);
    // Top at A line 64, then a page up (9 rows) ends on the pending row,
    // with line 56 under it.
    chars(&mut app, "g64\nb");
    draw(&mut app, &mut term);
    let before = rows_of(&term);
    assert!(before[3].contains("pending"), "{before:#?}");
    assert!(before[4].contains(" 56 "), "{before:#?}");
    while m.view.refine_next() {}
    draw(&mut app, &mut term);
    let after = rows_of(&term);
    assert_eq!(after[4], before[4], "{after:#?}");
}

#[test]
fn go_to_a_line_inside_a_pending_hunk_lands_on_it_once_refined() {
    let m = pending_mock();
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
    draw(&mut app, &mut term);
    chars(&mut app, "g50\n");
    draw(&mut app, &mut term);
    assert!(rows_of(&term)[3].contains("pending"));
    while m.view.refine_next() {}
    draw(&mut app, &mut term);
    let top = &rows_of(&term)[3];
    assert!(top.contains(" 50 "), "{top}");
    assert_eq!(last_visible(&m).start, 49);
}

#[test]
fn an_evicted_hunk_comes_back_at_the_same_row() {
    let m = Mock::build(
        &[
            Spec::Equal(40),
            Spec::Diff {
                del: 5,
                mid: 10,
                ins: 3,
            },
            Spec::Equal(55),
        ],
        true,
    );
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(60, 14)).unwrap();
    chars(&mut app, "g50\n");
    draw(&mut app, &mut term);
    let before = rows_of(&term)[3].clone();
    assert!(before.contains(" 50 "), "{before}");
    m.view.evict(1);
    draw(&mut app, &mut term);
    while m.view.refine_next() {}
    draw(&mut app, &mut term);
    assert_eq!(rows_of(&term)[3], before);
}
