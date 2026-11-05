//! The event loop and its key-to-frame instrumentation.

use std::io;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyEventKind};
use ratatui::Terminal;
use ratatui::backend::Backend;

use crate::app::{Action, App};

/// Key-to-frame durations, measured from the moment the loop receives a key
/// until `Terminal::draw` has flushed the frame.
///
/// Excludes the time the key spent in the terminal and the kernel before the
/// loop saw it.
#[derive(Debug)]
pub struct Latency {
    samples: Vec<Duration>,
}

impl Latency {
    /// Room for `n` samples, so recording never allocates inside the loop.
    #[must_use]
    pub fn with_capacity(n: usize) -> Self {
        Self {
            samples: Vec::with_capacity(n),
        }
    }

    /// Number of samples recorded.
    #[must_use]
    pub fn len(&self) -> usize {
        self.samples.len()
    }

    /// Whether nothing was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    fn record(&mut self, d: Duration) {
        self.samples.push(d);
    }

    /// Nearest-rank percentile, `p` in `0.0..=100.0`. Sorts in place.
    /// `None` when empty.
    #[must_use]
    pub fn percentile(&mut self, p: f64) -> Option<Duration> {
        if self.samples.is_empty() {
            return None;
        }
        self.samples.sort_unstable();
        let n = self.samples.len();
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "rank is within 1..=n, n far below 2^52"
        )]
        let rank = ((p / 100.0 * n as f64).ceil() as usize).clamp(1, n);
        Some(self.samples[rank - 1])
    }
}

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
/// pending hunk is on screen. If `latency` is given, each handled key adds
/// one sample.
///
/// # Errors
///
/// Returns the backend or event-source error.
pub fn run<B: Backend>(
    term: &mut Terminal<B>,
    app: &mut App<'_>,
    mut next_event: impl FnMut(Duration) -> Result<Option<Event>, B::Error>,
    mut latency: Option<&mut Latency>,
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
                if let Some(l) = latency.as_deref_mut() {
                    l.record(t0.elapsed());
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
