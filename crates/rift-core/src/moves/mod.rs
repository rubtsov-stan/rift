//! Moved blocks: runs of lines that appear in both files, at places the
//! skeleton's chain does not align.
//!
//! Only the type is here so far; detection is not written. A moved block
//! annotates the diff and never changes it: the edit script still deletes
//! the block on one side and inserts it on the other.

use std::ops::Range;

use crate::types::LineNo;

/// Lines of A that reappear, byte for byte and in the same order, as lines
/// of B. Both ranges hold the same number of lines.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MovedBlock {
    /// The block in A.
    pub a: Range<LineNo>,
    /// The block in B.
    pub b: Range<LineNo>,
}
