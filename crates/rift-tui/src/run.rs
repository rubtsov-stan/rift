//! The event loop and its key-to-frame instrumentation.

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::app::{Action, App};

/// Event source for a real terminal: waits up to `timeout` for an event.
///
/// # Errors
///
/// Returns the I/O error from crossterm.
pub fn crossterm_events(timeout: Duration) -> io::Result<Option<Event>> {
    if event::poll(timeout)? {
        event::read().map(Some)
    } else {
        Ok(None)
    }
}

/// Runs the interface until the user quits.
///
/// `next_event(tick)` returns the next event, or `None` after `tick` without
/// one. On every tick the frame is redrawn if refinement progressed or a
/// pending hunk is on screen. If `on_key_frame` is given, it receives, once per
/// handled key, the time from the loop receiving the key until its frame was
/// flushed (not counting the time the key spent in the terminal).
///
/// # Errors
///
/// Returns the backend or event-source error.
pub fn run<B: Backend>(
    term: &mut Terminal<B>,
    app: &mut App<'_>,
    mut next_event: impl FnMut(Duration) -> Result<Option<Event>, B::Error>,
    mut on_key_frame: Option<&mut dyn FnMut(Duration)>,
) -> Result<(), B::Error> {
    const TICK: Duration = Duration::from_millis(100);
    term.draw(|f| app.draw(f))?;
    loop {
        match next_event(TICK)? {
            Some(Event::Key(k)) if k.kind != KeyEventKind::Release => {
                let t0 = Instant::now();
                if app.handle_key(k) == Action::Quit {
                    return Ok(());
                }
                term.draw(|f| app.draw(f))?;
                if let Some(report) = on_key_frame.as_deref_mut() {
                    report(t0.elapsed());
                }
            }
            Some(Event::Resize(..)) => {
                term.draw(|f| app.draw(f))?;
            }
            Some(_) => {}
            None => {
                if app.needs_redraw() {
                    term.draw(|f| app.draw(f))?;
                }
            }
        }
    }
}
