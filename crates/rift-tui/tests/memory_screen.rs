//! Drawing a screen of very long lines costs a few kilobytes.
//!
//! Alone in its binary: the allocator counters are process-wide, and the
//! test harness allocates when it starts another test's thread.
#![allow(missing_docs)]

use std::ops::Range;
use std::sync::atomic::Ordering;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use rift_core::{Bytes, LineAccess, LineBuf, LineNo, Result};
use rift_tui::App;

mod common;

#[path = "common/counting.rs"]
mod counting;

use counting::{CURRENT, PEAK};

#[global_allocator]
static A: counting::Counting = counting::Counting;

/// Every line is 100 KB long.
struct Long;

impl LineAccess for Long {
    fn line_count(&self) -> u64 {
        1000
    }
    fn read_lines(&self, _: Range<LineNo>, _: &mut LineBuf) -> Result<()> {
        unreachable!("the interface reads prefixes only")
    }
    fn read_line_prefix(&self, _: LineNo, max: Bytes, buf: &mut LineBuf) -> Result<Bytes> {
        buf.clear();
        buf.push(&[b'z'; 4096][..usize::try_from(max.0).unwrap().min(4096)])?;
        Ok(Bytes(100_000))
    }

    fn read_line_part(&self, _: LineNo, _: Bytes, _: Bytes, _: &mut LineBuf) -> Result<bool> {
        unreachable!("the interface reads prefixes only")
    }
}

#[test]
fn a_full_screen_of_long_lines_costs_a_few_kilobytes() {
    let m = common::Mock::build(&[common::Spec::Equal(1000)], true);
    let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
    let base = CURRENT.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let mut app = App::new(&m.view, &Long, &Long);
    for _ in 0..3 {
        app.draw(&mut term.get_frame());
    }
    let peak = PEAK.load(Ordering::Relaxed) - base;
    println!("peak heap, 120x40 screen of 100 KB lines: {peak} B");
    assert!(peak < 64 << 10, "{peak} bytes");
}
