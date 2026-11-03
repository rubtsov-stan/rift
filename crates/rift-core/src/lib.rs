//! Memory-bounded line diff engine.
//!
//! The crate knows nothing about the terminal or the file system: input
//! arrives through [`Source`], and every data structure is sized from a byte
//! budget. See `DESIGN.md` in the repository root for the invariants.

mod source;

pub use source::Source;
