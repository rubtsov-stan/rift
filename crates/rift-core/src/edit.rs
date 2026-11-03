//! Edit script: the exact result of refining a segment.

use crate::types::LineNo;

/// One run of an edit script, in absolute line numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Edit {
    /// `len` lines of A starting at `a` equal `len` lines of B starting at `b`.
    Equal {
        /// First line in A.
        a: LineNo,
        /// First line in B.
        b: LineNo,
        /// Run length.
        len: u64,
    },
    /// `len` lines of A starting at `a` are absent from B.
    Delete {
        /// First line in A.
        a: LineNo,
        /// Run length.
        len: u64,
    },
    /// `len` lines of B starting at `b` are absent from A.
    Insert {
        /// First line in B.
        b: LineNo,
        /// Run length.
        len: u64,
    },
}

/// The edit script for one segment.
///
/// Invariant: the edits are ordered and contiguous. Walking them in order
/// visits every line of the segment's A range exactly once (through `Equal`
/// and `Delete`) and every line of its B range exactly once (through `Equal`
/// and `Insert`), each in increasing order. Replaying them against A yields
/// B. A hunk with `approx` set is still exact in this sense; it is only not
/// minimal.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Hunk {
    /// The edits, in order.
    pub edits: Vec<Edit>,
    /// The local diff hit its cost cutoff; the script is valid but not minimal.
    pub approx: bool,
}
