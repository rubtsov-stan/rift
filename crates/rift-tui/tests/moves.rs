//! Moved blocks on screen: both sides marked, `m` jumps between them.
#![allow(missing_docs)]

mod common;

use common::{Mock, Spec};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rift_core::moves::MovedBlock;
use rift_core::{LineNo, MemLines};
use rift_tui::App;

/// A lines 40..45 deleted, and inserted again as B lines 100..105 after
/// 60 shared lines, recorded as one moved block; two more deleted lines
/// at A 105..107 that did not move.
fn mock() -> Mock {
    let mut m = Mock::build(
        &[
            Spec::Equal(40),
            Spec::Diff {
                del: 5,
                mid: 0,
                ins: 0,
            },
            Spec::Equal(60),
            Spec::Diff {
                del: 2,
                mid: 0,
                ins: 5,
            },
            Spec::Equal(40),
        ],
        true,
    );
    m.view.moved = vec![MovedBlock {
        a: LineNo(40)..LineNo(45),
        b: LineNo(100)..LineNo(105),
    }];
    m
}

fn rows(app: &mut App<'_>, term: &mut Terminal<TestBackend>) -> Vec<String> {
    term.draw(|f| app.draw(f)).unwrap();
    let buf = term.backend().buffer();
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect())
        .collect()
}

fn press(app: &mut App<'_>, c: char) {
    app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
}

/// The first row of the diff area, below the three minimap rows.
const TOP: usize = 3;

#[test]
fn both_sides_are_marked_and_m_jumps_between_them() {
    let m = mock();
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(70, 16)).unwrap();

    press(&mut app, 'n');
    let screen = rows(&mut app, &mut term);
    assert!(screen[TOP].starts_with("m-  41"), "{screen:#?}");
    assert!(screen[TOP + 4].starts_with("m-  45"), "{screen:#?}");
    assert!(screen.last().unwrap().contains("moved 1"), "{screen:#?}");

    press(&mut app, 'm');
    let screen = rows(&mut app, &mut term);
    assert!(screen[TOP].starts_with("m+     101"), "{screen:#?}");
    assert!(screen[TOP + 4].starts_with("m+     105"), "{screen:#?}");

    press(&mut app, 'm');
    let screen = rows(&mut app, &mut term);
    assert!(screen[TOP].starts_with("m-  41"), "{screen:#?}");
}

#[test]
fn rows_outside_a_block_are_not_marked() {
    let m = mock();
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(70, 16)).unwrap();
    press(&mut app, 'n');
    press(&mut app, 'n');
    let screen = rows(&mut app, &mut term);
    // The two deleted lines that did not move, then the inserted block.
    assert!(screen[TOP].starts_with(" - 106"), "{screen:#?}");
    assert!(screen[TOP + 2].starts_with("m+     101"), "{screen:#?}");
}

#[test]
fn m_without_a_moved_row_on_screen_says_so() {
    let m = mock();
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    let mut term = Terminal::new(TestBackend::new(70, 16)).unwrap();
    rows(&mut app, &mut term);
    press(&mut app, 'm');
    let screen = rows(&mut app, &mut term);
    assert!(screen[TOP].starts_with("     1   1"), "{screen:#?}");
    assert!(
        screen.last().unwrap().contains("no moved block on screen"),
        "{screen:#?}"
    );
}
