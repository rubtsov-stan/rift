//! Try the interface on a generated diff.
//!
//! `cargo run -p rift-tui --example mock`          interactive, hunks refine in the background
//! `cargo run --release -p rift-tui --example mock -- --bench [keys]`
//!                                                  key-to-frame latency, p50 and p99
#![allow(missing_docs, clippy::many_single_char_names)]

#[path = "../tests/common/mod.rs"]
mod common;

use std::io;
use std::time::Duration;

use common::Mock;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};
use rift_core::MemLines;
use rift_tui::{App, Latency, crossterm_events, run};

const SEGMENTS: usize = 50_000;

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let bench = args.iter().position(|a| a == "--bench");
    let m = Mock::large(SEGMENTS, bench.is_some());
    let (a, b) = (MemLines::new(&m.a), MemLines::new(&m.b));
    let mut app = App::new(&m.view, &a, &b);
    match bench {
        Some(i) => {
            let n = args
                .get(i + 1)
                .and_then(|s| s.parse().ok())
                .unwrap_or(20_000);
            bench_keys(&mut app, n)
        }
        None => interactive(&m, &mut app),
    }
}

fn interactive(m: &Mock, app: &mut App<'_>) -> io::Result<()> {
    let mut term = ratatui::init();
    let done = std::sync::atomic::AtomicBool::new(false);
    let res = std::thread::scope(|s| {
        s.spawn(|| {
            while !done.load(std::sync::atomic::Ordering::Relaxed) && m.view.refine_next() {
                std::thread::sleep(Duration::from_millis(40));
            }
        });
        let r = run(&mut term, app, crossterm_events, None);
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        r
    });
    ratatui::restore();
    res
}

/// Scripted keys through the real loop and the real crossterm backend,
/// writing escape sequences to a sink. The terminal is a fixed 200x50 area.
fn bench_keys(app: &mut App<'_>, n: usize) -> io::Result<()> {
    let backend = CrosstermBackend::new(io::sink());
    let mut term = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 200, 50)),
        },
    )?;
    let script = ['n', ' ', 'n', 'p', 'b', 'n', ' ', ' '];
    let mut sent = 0;
    let mut lat = Latency::with_capacity(n);
    run(
        &mut term,
        app,
        |_| {
            sent += 1;
            let code = if sent > n {
                KeyCode::Char('q')
            } else {
                KeyCode::Char(script[sent % script.len()])
            };
            Ok(Some(Event::Key(KeyEvent::new(code, KeyModifiers::NONE))))
        },
        Some(&mut lat),
    )?;
    let us = |d: Duration| d.as_secs_f64() * 1e6;
    println!(
        "{} keys, 200x50: p50 {:.0} us, p99 {:.0} us, max {:.0} us",
        lat.len(),
        us(lat.percentile(50.0).unwrap()),
        us(lat.percentile(99.0).unwrap()),
        us(lat.percentile(100.0).unwrap()),
    );
    Ok(())
}
