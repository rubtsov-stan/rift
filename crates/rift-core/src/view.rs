//! What a front end sees: the segment map, hunks on demand, progress.

use std::ops::Range;
use std::sync::Arc;

use crate::edit::Hunk;
use crate::moves::MovedBlock;
use crate::segment::Segment;
use crate::types::LineNo;

/// Refinement state of one segment.
#[derive(Debug, Clone)]
pub enum HunkState {
    /// Exact script available.
    Ready(Arc<Hunk>),
    /// Not computed yet, or evicted and being recomputed.
    Pending,
    /// Script available but cut short by the cost cutoff.
    Approx(Arc<Hunk>),
}

/// Background work done so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Progress {
    /// Segments refined.
    pub done: u64,
    /// Segments that need refinement.
    pub total: u64,
}

/// A diff that computes its hunks lazily.
///
/// The front end calls [`set_visible`](DiffView::set_visible) whenever the
/// viewport moves; the implementation prioritizes those segments and their
/// neighbours. [`hunk`](DiffView::hunk) never blocks: it returns
/// [`HunkState::Pending`] and the front end polls again on the next frame.
pub trait DiffView: Send + Sync {
    /// Lines in A.
    fn len_a(&self) -> u64;

    /// Lines in B.
    fn len_b(&self) -> u64;

    /// The segment map, in file order.
    fn segments(&self) -> &[Segment];

    /// State of the hunk for `segments()[segment]`.
    ///
    /// # Panics
    ///
    /// May panic if `segment` is out of range.
    fn hunk(&self, segment: usize) -> HunkState;

    /// Background progress.
    fn progress(&self) -> Progress;

    /// Lines of A currently on screen. Inserted lines count with the A line
    /// they follow.
    fn set_visible(&self, range: Range<LineNo>);

    /// Blocks that moved, ordered by their line in A. Empty unless the
    /// implementation detects moves.
    fn moved_blocks(&self) -> &[MovedBlock] {
        &[]
    }
}
