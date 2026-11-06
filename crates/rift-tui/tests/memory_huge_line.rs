//! Drawing a 64 MiB line never reads it whole.
//!
//! Alone in its binary: the allocator counters are process-wide, and the
//! test harness allocates when it starts another test's thread.
#![allow(missing_docs)]

use std::sync::atomic::Ordering;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rift_tui::App;

mod common;

#[path = "common/counting.rs"]
mod counting;

use counting::{CURRENT, PEAK};

#[global_allocator]
static A: counting::Counting = counting::Counting;

#[test]
fn a_huge_line_is_never_read_whole() {
    let mut a = b"first\n".to_vec();
    a.extend(std::iter::repeat_n(b'y', 64 << 20));
    a.extend_from_slice(b"\nlast\n");
    let lines = rift_core::MemLines::new(&a);
    let m = common::Mock::build(&[common::Spec::Equal(3)], true);
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let base = CURRENT.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let mut app = App::new(&m.view, &lines, &lines);
    app.draw(&mut term.get_frame());
    let peak = PEAK.load(Ordering::Relaxed) - base;
    println!("peak heap drawing a 64 MiB line: {peak} B");
    assert!(peak < 1 << 20, "{peak} bytes");
}
