//! Keys, visible-range reporting and the event loop, on the mock view.
#![allow(missing_docs)]

mod common;

use std::collections::VecDeque;
use std::convert::Infallible;

use common::{Mock, Spec};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rift_core::MemLines;
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
