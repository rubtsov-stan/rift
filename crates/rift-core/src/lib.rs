//! Memory-bounded line diff engine.
//!
//! The crate knows nothing about the terminal or the file system: input
//! arrives through [`Source`], and every data structure is sized from a byte
//! [`Budget`]. See `DESIGN.md` in the repository root for the invariants and
//! the meaning of every contract type re-exported here.

#[cfg(feature = "counting-alloc")]
pub mod alloc;
pub mod budget;
mod edit;
mod error;
pub mod lines;
mod sample;
mod segment;
mod source;
pub mod types;
mod view;

pub mod anchors;
pub mod diff;
pub mod index;
pub mod refine;
pub mod scan;
pub mod sched;
pub mod script;
pub mod skeleton;
pub mod strata;
pub mod unified;

pub use budget::{Budget, BudgetExceeded, Half, Reservation};
pub use edit::{Edit, Hunk};
pub use error::{Error, Result};
pub use lines::{LineAccess, LineBuf, MemLines};
pub use sample::Sample;
pub use segment::{Segment, SegmentStatus};
pub use source::Source;
pub use types::{ByteOffset, Bytes, LineHash, LineNo, ParseBytesError};
pub use view::{DiffView, HunkState, Progress};
