//! Moved blocks: runs of lines that appear in both files, at places the
//! skeleton's chain does not align.
//!
//! A block that moved shows up in the diff as a deletion and an insertion.
//! Its unique lines still match between the files, and the chain search
//! leaves those matches out because they cross the chain: they are the
//! skeleton's [`OffChain`](crate::skeleton::OffChain) pairs. [`Moves::find`]
//! takes each pair as a seed, compares its two lines byte by byte (a pair
//! is only a hash match), and extends the run line by line in both
//! directions while the lines stay equal and outside `Equal` segments.
//! Runs of at least [`MIN_LINES`] lines are kept. A seed inside a block
//! already found adds nothing, so consecutive pairs group into one run.
//!
//! A moved block annotates the diff and never changes it: the edit script
//! still deletes the block on one side and inserts it on the other.
//!
//! Seeds come from the skeleton's samples, so at stratum `L` a block of
//! `n` lines is missed when none of its lines was sampled, with chance
//! about `(1 - 2^-L)^n`. Every block reported is exact.

#[cfg(test)]
mod tests;

use std::ops::Range;

use crate::budget::{Budget, Half, Reservation};
use crate::error::{Error, Result};
use crate::lines::{LineAccess, LineBuf};
use crate::refine::same_line;
use crate::scan::release_slack;
use crate::script::PIECE;
use crate::segment::{Segment, SegmentStatus};
use crate::types::{Bytes, LineNo};

/// Lines of A that reappear, byte for byte and in the same order, as lines
/// of B. Both ranges hold the same number of lines.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MovedBlock {
    /// The block in A.
    pub a: Range<LineNo>,
    /// The block in B.
    pub b: Range<LineNo>,
}

/// The shortest run reported as a moved block. Shorter runs of a unique
/// line and its neighbours turn up in data without moves, such as a
/// changed row that happens to equal a row elsewhere.
pub const MIN_LINES: u64 = 3;

const BLOCK_BYTES: u64 = size_of::<MovedBlock>() as u64;
/// Lines per side read at once while a run is extended.
const READ_LINES: u64 = 16;

/// The two comparison buffers, for readers that return lines of up to
/// `line_cap` bytes whole: each holds [`READ_LINES`] such lines, or one
/// piece of a longer line, at most [`PIECE`] bytes, and may double while
/// it grows to that.
fn work(line_cap: Bytes) -> Bytes {
    let one = (READ_LINES * line_cap.0).max(PIECE.0) + READ_LINES * size_of::<usize>() as u64;
    Bytes(2 * 2 * one)
}
/// The block list takes at most this fraction, `1 / BLOCK_SHARE`, of the
/// refine half that is free when detection starts.
const BLOCK_SHARE: u64 = 8;
/// The block list grows by at least this many blocks at a time.
const BLOCK_STEP: usize = 16;

/// Moved blocks of a diff, ordered by their line in A, or the note that
/// the budget did not allow looking for them.
///
/// Memory, from the refine half: 32 bytes per block for as long as the
/// list lives, at most `1 / 8` of what that half has free when detection
/// starts, and while it runs two comparison buffers of 16 lines, or of
/// 64 KiB when that is more, doubled for growth: 256 KiB for readers that
/// return lines of up to 4 KiB whole.
/// When either does not fit, detection stops, keeps nothing and reports
/// itself [`skipped`](Self::skipped).
#[derive(Debug)]
pub struct Moves {
    blocks: Vec<MovedBlock>,
    skipped: bool,
    _reservation: Option<Reservation>,
}

impl Moves {
    /// Finds the moved blocks seeded by `pairs`, `[line in A, line in B]`
    /// in A order, over the segment map `segments` of the files `a` and
    /// `b`, whose readers return lines of up to `line_cap` bytes whole.
    ///
    /// Reads 16 lines per side and seed and then as a block extends, and
    /// each line of A at most once as part of a block: `O(pairs + moved
    /// lines)` line comparisons, plus `O(log segments)` per comparison.
    ///
    /// # Errors
    ///
    /// Errors from reading either file. A budget too small is not an error:
    /// the result is [`skipped`](Self::skipped).
    pub fn find(
        pairs: &[[LineNo; 2]],
        segments: &[Segment],
        a: &impl LineAccess,
        b: &impl LineAccess,
        line_cap: Bytes,
        budget: &Budget,
    ) -> Result<Self> {
        let skipped = Self {
            blocks: Vec::new(),
            skipped: true,
            _reservation: None,
        };
        let limit = budget.available(Half::Refine).0 / BLOCK_SHARE / BLOCK_BYTES;
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        let Ok(_work) = budget.try_reserve(Half::Refine, work(line_cap)) else {
            return Ok(skipped);
        };
        let Ok(mut held) = budget.try_reserve(Half::Refine, Bytes(0)) else {
            return Ok(skipped);
        };
        let mut run = Run {
            segments,
            a,
            b,
            buf_a: LineBuf::new(),
            buf_b: LineBuf::new(),
        };
        let mut blocks: Vec<MovedBlock> = Vec::new();
        for &[x, y] in pairs {
            let floor = blocks.last().map_or(LineNo(0), |l| l.a.end);
            if x < floor {
                continue;
            }
            // The seed is the first line compared going forward.
            let room = (a.line_count() - x.0).min(b.line_count() - y.0);
            let ahead = run.extend(x, y, true, room)?;
            if ahead == 0 {
                continue;
            }
            let back = run.extend(x, y, false, (x - floor).min(y.0))?;
            if back + ahead < MIN_LINES {
                continue;
            }
            if blocks.len() == blocks.capacity() {
                let len = blocks.len();
                let extra = (2 * len).max(BLOCK_STEP).min(limit).saturating_sub(len);
                let bytes = Bytes(extra as u64 * BLOCK_BYTES);
                if extra == 0 || held.grow(bytes).is_err() {
                    return Ok(skipped);
                }
                if blocks.try_reserve_exact(extra).is_err() {
                    return Ok(skipped);
                }
            }
            blocks.push(MovedBlock {
                a: LineNo(x.0 - back)..x + ahead,
                b: LineNo(y.0 - back)..y + ahead,
            });
        }
        release_slack(&mut blocks, &mut held);
        Ok(Self {
            blocks,
            skipped: false,
            _reservation: Some(held),
        })
    }

    /// The blocks, ordered by their line in A.
    #[must_use]
    pub fn blocks(&self) -> &[MovedBlock] {
        &self.blocks
    }

    /// Whether the budget did not allow detection, so moves, if any, are
    /// not listed.
    #[must_use]
    pub fn skipped(&self) -> bool {
        self.skipped
    }
}

/// What extending a run reads through.
struct Run<'s, A, B> {
    segments: &'s [Segment],
    a: &'s A,
    b: &'s B,
    buf_a: LineBuf,
    buf_b: LineBuf,
}

impl<A: LineAccess, B: LineAccess> Run<'_, A, B> {
    /// How many line pairs, at most `max`, can be part of one moved block
    /// with A line `x` and B line `y`: from them forward, or from the pair
    /// before them backward. A pair can when neither line lies in an
    /// `Equal` segment, where the diff already aligns it, and their bytes
    /// are the same.
    fn extend(&mut self, x: LineNo, y: LineNo, forward: bool, max: u64) -> Result<u64> {
        let mut done = 0;
        while done < max {
            let k = (max - done).min(READ_LINES);
            let (sa, sb) = if forward {
                (x + done, y + done)
            } else {
                (LineNo(x.0 - done - k), LineNo(y.0 - done - k))
            };
            let whole = whole_lines(self.a, sa..sa + k, &mut self.buf_a)?
                && whole_lines(self.b, sb..sb + k, &mut self.buf_b)?;
            for j in 0..k {
                let i = if forward { j } else { k - 1 - j };
                let (la, lb) = (sa + i, sb + i);
                let same = !self.equal(la, |s| &s.a)
                    && !self.equal(lb, |s| &s.b)
                    && if whole {
                        let i = usize::try_from(i).expect("i < READ_LINES");
                        self.buf_a.line(i) == self.buf_b.line(i)
                    } else {
                        same_line(self.a, la, self.b, lb, &mut self.buf_a, &mut self.buf_b)?
                    };
                if !same {
                    return Ok(done + j);
                }
            }
            done += k;
        }
        Ok(max)
    }

    /// Whether `line` lies in an `Equal` segment, on the side `side` picks.
    fn equal(&self, line: LineNo, side: impl Fn(&Segment) -> &Range<LineNo>) -> bool {
        let i = self.segments.partition_point(|s| side(s).end <= line);
        self.segments
            .get(i)
            .is_some_and(|s| s.status == SegmentStatus::Equal)
    }
}

/// Reads `range` into `buf`; false when a line is too long for the reader
/// and the block has to be compared pair by pair in pieces.
fn whole_lines(lines: &impl LineAccess, range: Range<LineNo>, buf: &mut LineBuf) -> Result<bool> {
    match lines.read_lines(range, buf) {
        Ok(()) => Ok(true),
        Err(Error::LineTooLong { .. }) => Ok(false),
        Err(e) => Err(e),
    }
}
