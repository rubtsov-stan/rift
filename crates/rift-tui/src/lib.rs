//! Terminal interface for Rift.
//!
//! The interface sees the diff only through [`rift_core::DiffView`] and the line text
//! only through two [`rift_core::LineAccess`] handles (`DiffView` has no line-text
//! method). It never blocks on refinement: a hunk that is not ready is drawn
//! as a pending row and the frame is redrawn on the next tick.
//!
//! Memory: nothing is kept per segment. Segment data is read from the view
//! on demand, and the interface holds two running sums and `O(width)`
//! minimap cells, so its footprint does not depend on the file size.
#![forbid(unsafe_code)]

mod app;
mod cache;
mod hidden_table;
mod minimap;
mod render;
mod run;

pub use app::{Action, App};
pub use run::{crossterm_events, run};
