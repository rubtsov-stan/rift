//! Snapshot tests: every screen state drawn on a `TestBackend`.
#![allow(missing_docs)]

mod common;

use common::{Mock, Spec};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rift_core::MemLines;
use rift_tui::{Action, App};

const W: u16 = 56;
const H: u16 = 12;

fn specs() -> [Spec; 5] {
    [
        Spec::Equal(3),
        Spec::Diff {
            del: 2,
            mid: 1,
            ins: 1,
        },
        Spec::Equal(2),
        Spec::Approx {
            del: 1,
            mid: 0,
            ins: 2,
        },
        Spec::Equal(2),
    ]
}

fn key(c: KeyCode) -> KeyEvent {
    KeyEvent::new(c, KeyModifiers::NONE)
}

fn press(app: &mut App<'_>, keys: &str) {
    for c in keys.chars() {
        let code = if c == '\n' {
            KeyCode::Enter
        } else {
            KeyCode::Char(c)
        };
        assert_eq!(app.handle_key(key(code)), Action::Continue);
    }
}

fn screen(app: &mut App<'_>, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    let buf = term.backend().buffer();
    let mut out = String::new();
    for y in 0..h {
        let line: String = (0..w).map(|x| buf[(x, y)].symbol()).collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn with_app<R>(m: &Mock, f: impl FnOnce(&mut App<'_>) -> R) -> R {
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    f(&mut App::new(&m.view, &a, &b))
}

#[test]
fn refined_top() {
    let m = Mock::build(&specs(), true);
    with_app(&m, |app| {
        assert_eq!(
            screen(app, W, H),
            r"D ·············█····█···█·······························
~ ····································█···█·············
v ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀··························
    1  1 │ line 0000000 the quick brown fox
    2  2 │ line 0000001 the quick brown fox
    3  3 │ line 0000002 the quick brown fox
 -  4    │ line 0000003 was removed here
 -  5    │ line 0000004 was removed here
    6  4 │ line 0000005 the quick brown fox
 +     5 │ line 0000004 was added here
    7  6 │ line 0000006 the quick brown fox
 A 11  B 11  diff 0/2                       refined 2/2
"
        );
    });
}

#[test]
fn first_difference() {
    let m = Mock::build(&specs(), true);
    with_app(&m, |app| {
        press(app, "n");
        assert_eq!(
            screen(app, W, H),
            r"D ·············█····█···█·······························
~ ····································█···█·············
v ·············▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀·············
 -  4    │ line 0000003 was removed here
 -  5    │ line 0000004 was removed here
    6  4 │ line 0000005 the quick brown fox
 +     5 │ line 0000004 was added here
    7  6 │ line 0000006 the quick brown fox
    8  7 │ line 0000007 the quick brown fox
~-  9    │ line 0000008 was removed here
~+     8 │ line 0000007 was added here
 A 11  B 11  diff 1/2                       refined 2/2
"
        );
    });
}

#[test]
fn approx_hunk() {
    let m = Mock::build(&specs(), true);
    with_app(&m, |app| {
        press(app, "nn");
        assert_eq!(
            screen(app, W, H),
            r"D ·············█····█···█·······························
~ ····································█···█·············
v ····································▀▀▀▀▀▀▀▀▀▀▀▀▀▀····
~-  9    │ line 0000008 was removed here
~+     8 │ line 0000007 was added here
~+     9 │ line 0000008 was added here
   10 10 │ line 0000009 the quick brown fox
   11 11 │ line 0000010 the quick brown fox



 A 11  B 11  diff 2/2                       refined 2/2
"
        );
    });
}

#[test]
fn no_next_difference() {
    let m = Mock::build(&specs(), true);
    with_app(&m, |app| {
        press(app, "nnn");
        assert_eq!(
            screen(app, W, H),
            r"D ·············█····█···█·······························
~ ····································█···█·············
v ····································▀▀▀▀▀▀▀▀▀▀▀▀▀▀····
~-  9    │ line 0000008 was removed here
~+     8 │ line 0000007 was added here
~+     9 │ line 0000008 was added here
   10 10 │ line 0000009 the quick brown fox
   11 11 │ line 0000010 the quick brown fox



 A 11  B 11  diff 2/2  no next difference   refined 2/2
"
        );
    });
}

#[test]
fn pending() {
    let m = Mock::build(&specs(), false);
    with_app(&m, |app| {
        assert_eq!(
            screen(app, W, H),
            r"D ·············█····█···█·······························
~ ····································█···█·············
v ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀········
    1  1 │ line 0000000 the quick brown fox
    2  2 │ line 0000001 the quick brown fox
    3  3 │ line 0000002 the quick brown fox
  ? pending  A 4..6  B 4..5
    7  6 │ line 0000006 the quick brown fox
    8  7 │ line 0000007 the quick brown fox
  ? pending  A 9..9  B 8..9
   10 10 │ line 0000009 the quick brown fox
 A 11  B 11  diff 0/2         refining 0/2 [░░░░░░░░░░]
"
        );
    });
}

#[test]
fn partial_progress() {
    let m = Mock::build(&specs(), false);
    with_app(&m, |app| {
        m.view.refine_next();
        assert_eq!(
            screen(app, W, H),
            r"D ·············█····█···█·······························
~ ····································█···█·············
v ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀··························
    1  1 │ line 0000000 the quick brown fox
    2  2 │ line 0000001 the quick brown fox
    3  3 │ line 0000002 the quick brown fox
 -  4    │ line 0000003 was removed here
 -  5    │ line 0000004 was removed here
    6  4 │ line 0000005 the quick brown fox
 +     5 │ line 0000004 was added here
    7  6 │ line 0000006 the quick brown fox
 A 11  B 11  diff 0/2         refining 1/2 [█████░░░░░]
"
        );
    });
}

#[test]
fn goto_prompt() {
    let m = Mock::build(&specs(), false);
    with_app(&m, |app| {
        m.view.refine_next();
        press(app, "g1");
        assert_eq!(
            screen(app, W, H),
            r"D ·············█····█···█·······························
~ ····································█···█·············
v ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀··························
    1  1 │ line 0000000 the quick brown fox
    2  2 │ line 0000001 the quick brown fox
    3  3 │ line 0000002 the quick brown fox
 -  4    │ line 0000003 was removed here
 -  5    │ line 0000004 was removed here
    6  4 │ line 0000005 the quick brown fox
 +     5 │ line 0000004 was added here
    7  6 │ line 0000006 the quick brown fox
 go to line: 1_  Enter/Esc    refining 1/2 [█████░░░░░]
"
        );
    });
}

#[test]
fn goto_result() {
    let m = Mock::build(&specs(), false);
    with_app(&m, |app| {
        m.view.refine_next();
        press(app, "g10\n");
        assert_eq!(
            screen(app, W, H),
            r"D ·············█····█···█·······························
~ ····································█···█·············
v ·············································▀▀▀▀▀····
   10 10 │ line 0000009 the quick brown fox
   11 11 │ line 0000010 the quick brown fox






 A 11  B 11  diff 2/2         refining 1/2 [█████░░░░░]
"
        );
    });
}

#[test]
fn too_small() {
    let m = Mock::build(&specs(), true);
    with_app(&m, |app| {
        assert_eq!(
            screen(app, 20, 2),
            r"terminal too small

"
        );
    });
}

#[test]
fn no_minimap_when_short() {
    let m = Mock::build(&specs(), true);
    with_app(&m, |app| {
        assert_eq!(
            screen(app, 40, 6),
            r"    1  1 │ line 0000000 the quick brown
    2  2 │ line 0000001 the quick brown
    3  3 │ line 0000002 the quick brown
 -  4    │ line 0000003 was removed here
 -  5    │ line 0000004 was removed here
 A 11  B 11  diff 0/2       refined 2/2
"
        );
    });
}

#[test]
fn identical() {
    let m = Mock::build(&[Spec::Equal(3)], true);
    with_app(&m, |app| {
        assert_eq!(
            screen(app, W, 8),
            r"   1 1 │ line 0000000 the quick brown fox
   2 2 │ line 0000001 the quick brown fox
   3 3 │ line 0000002 the quick brown fox




 A 3  B 3  diff 0/0  files are identical
"
        );
    });
}

#[test]
fn empty() {
    let m = Mock::build(&[], true);
    with_app(&m, |app| {
        assert_eq!(
            screen(app, W, 8),
            r"  both files are empty






 A 0  B 0  diff 0/0  files are identical
"
        );
    });
}
