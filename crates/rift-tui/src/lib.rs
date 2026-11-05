//! Terminal interface for Rift.
//!
//! The interface sees the diff only through [`rift_core::DiffView`] and the line text
//! only through two [`rift_core::LineAccess`] handles (`DiffView` has no line-text
//! method). It never blocks on refinement: a hunk that is not ready is drawn
//! as a pending row and the frame is redrawn on the next tick.
//!
//! Memory: the interface keeps `O(m)` small vectors over the segment list
//! (`diffs`, `starts`) and `O(width)` minimap cells. They sit outside the
//! engine budget and are about 16 bytes per segment.
#![forbid(unsafe_code)]

mod app;
mod minimap;
mod render;
mod run;

pub use app::{Action, App};
pub use run::{Latency, crossterm_events, run};
