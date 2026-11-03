//! Segments of the global match: what lies between two anchors.

use std::ops::Range;

use crate::types::LineNo;

/// How much is known about a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SegmentStatus {
    /// The byte ranges are identical, confirmed byte by byte.
    Equal,
    /// The ranges differ; an exact local diff is possible.
    Differs,
    /// The ranges differ and the local diff had to be cut short.
    Approx,
}

/// A pair of line ranges, one per file, with the same meaning in both.
///
/// Consecutive segments tile both files without gaps or overlap: the `a`
/// ranges concatenate to `0..len_a`, the `b` ranges to `0..len_b`. Either
/// range may be empty, never both.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Segment {
    /// Lines of A.
    pub a: Range<LineNo>,
    /// Lines of B.
    pub b: Range<LineNo>,
    /// What is known.
    pub status: SegmentStatus,
}

impl Segment {
    /// Lines of A in the segment.
    #[must_use]
    pub fn len_a(&self) -> u64 {
        self.a.end - self.a.start
    }

    /// Lines of B in the segment.
    #[must_use]
    pub fn len_b(&self) -> u64 {
        self.b.end - self.b.start
    }
}
