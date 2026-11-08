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
    /// Why part of the script may not be minimal; `None` when it is.
    pub approx: Option<ApproxReason>,
}

/// Why a hunk is approximate. When a hunk has several, it keeps the
/// greatest, so the reason a larger `--mem` can fix wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ApproxReason {
    /// A gap had lines on both sides but none once on each, and the Myers
    /// search passed its cost cutoff.
    Cutoff,
    /// A segment too large for one job had no anchor among its samples,
    /// and its two sides share a line hash.
    NoAnchor,
    /// The job ran out of its share of `--mem`: for samples, for the edits,
    /// or for anything at all.
    Budget,
}

impl ApproxReason {
    /// Every reason, in order.
    pub const ALL: [Self; 3] = [Self::Cutoff, Self::NoAnchor, Self::Budget];

    /// The reason in plain words, for a person reading the diff.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Cutoff => "too many changes among repeated lines to align them exactly",
            Self::NoAnchor => {
                "found no line here that occurs once in each file, so there is nothing to align on"
            }
            Self::Budget => "too large to align within --mem; a larger --mem may help",
        }
    }
}
