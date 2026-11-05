//! Snapshot tests: every screen state drawn on a `TestBackend`.
#![allow(missing_docs)]

mod common;

use common::{Mock, Spec};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rift_core::scan::{ScanConfig, scan};
use rift_core::{
    Budget, Bytes, Half, LineAccess, LineBuf, LineNo, MemLines, Result, index::IndexedLines,
};
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

#[test]
fn paging_past_the_end_keeps_the_last_line_at_the_bottom() {
    let m = Mock::build(&specs(), true);
    with_app(&m, |app| {
        // The first frame tells the app how tall the view is.
        screen(app, W, H);
        for _ in 0..5 {
            app.handle_key(key(KeyCode::PageDown));
        }
        assert_eq!(
            screen(app, W, H),
            r"D ·············█····█···█·······························
~ ····································█···█·············
v ······················▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀····
 +     5 │ line 0000004 was added here
    7  6 │ line 0000006 the quick brown fox
    8  7 │ line 0000007 the quick brown fox
~-  9    │ line 0000008 was removed here
~+     8 │ line 0000007 was added here
~+     9 │ line 0000008 was added here
   10 10 │ line 0000009 the quick brown fox
   11 11 │ line 0000010 the quick brown fox
 A 11  B 11  diff 1/2                       refined 2/2
"
        );
    });
}

/// Serves the given bytes for every line, or fails.
struct Canned<'a> {
    lines: Vec<&'a [u8]>,
    fail: Option<&'static str>,
}

impl LineAccess for Canned<'_> {
    fn line_count(&self) -> u64 {
        self.lines.len() as u64
    }
    fn read_lines(&self, range: std::ops::Range<LineNo>, buf: &mut LineBuf) -> Result<()> {
        buf.clear();
        for i in range.start.0..range.end.0 {
            buf.push(self.lines[usize::try_from(i).unwrap()])?;
        }
        Ok(())
    }
    fn read_line_prefix(&self, line: LineNo, max: Bytes, buf: &mut LineBuf) -> Result<Bytes> {
        buf.clear();
        if let Some(msg) = self.fail {
            return Err(std::io::Error::other(msg).into());
        }
        let bytes = self.lines[usize::try_from(line.0).unwrap()];
        buf.push(&bytes[..bytes.len().min(usize::try_from(max.0).unwrap())])?;
        Ok(Bytes(bytes.len() as u64))
    }
}

fn show(lines: &Canned<'_>) -> String {
    let m = Mock::build(&[Spec::Equal(lines.lines.len() as u64)], true);
    let mut app = App::new(&m.view, lines, lines);
    screen(&mut app, 70, 8)
}

#[test]
fn file_text_is_shown_as_visible_escapes() {
    let lines = Canned {
        lines: vec![
            "a\u{202E}b\n".as_bytes(),
            "c\u{2066}d\u{200B}e\u{FEFF}\n".as_bytes(),
            b"f\x1b[31mg\x07\n",
        ],
        fail: None,
    };
    let s = show(&lines);
    assert!(s.contains(r"a\u{202E}b"), "{s}");
    assert!(s.contains(r"c\u{2066}d\u{200B}e\u{FEFF}"), "{s}");
    assert!(s.contains(r"f\u{1B}[31mg\u{7}"), "{s}");
    for raw in ['\u{202E}', '\u{2066}', '\u{200B}', '\u{FEFF}', '\x1b', '·'] {
        assert!(!s.contains(raw), "{raw:?} drawn raw:\n{s}");
    }
}

#[test]
fn error_text_is_sanitized() {
    let lines = Canned {
        lines: vec![b"x\n"],
        fail: Some("boom \u{202E}\x1b[31m"),
    };
    let s = show(&lines);
    assert!(s.contains(r"boom \u{202E}\u{1B}[31m"), "{s}");
    assert!(!s.contains('\u{202E}') && !s.contains('\x1b'), "{s}");
}

#[test]
fn a_line_over_the_budget_is_drawn_from_its_prefix() {
    // 1 MiB line, 256-byte read buffer: read_lines would fail on it.
    let mut data = b"before\n".to_vec();
    data.extend(std::iter::repeat_n(b'x', 1 << 20));
    data.extend_from_slice(b"\nafter\n");
    let budget = Budget::new(Bytes::mib(64));
    let (cfg, _) = ScanConfig::for_pair(Bytes(data.len() as u64), Bytes(0), &budget);
    let scanned = scan(&data[..], &budget, &cfg).unwrap();
    let lines = IndexedLines::new(
        &data[..],
        scanned.index(),
        &budget,
        Half::Refine,
        Bytes(256),
    )
    .unwrap();
    let mut probe = LineBuf::new();
    assert!(lines.read_lines(LineNo(1)..LineNo(2), &mut probe).is_err());
    let m = Mock::build(&[Spec::Equal(3)], true);
    let mut app = App::new(&m.view, &lines, &lines);
    let s = screen(&mut app, 70, 8);
    assert!(s.contains("before") && s.contains("after"), "{s}");
    assert!(s.contains(&"x".repeat(40)), "{s}");
    assert!(!s.contains("exceeds"), "{s}");
}
