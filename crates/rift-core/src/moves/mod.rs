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
//! Blocks never share a line of A, and a block that would share lines of B
//! with another is clipped, so every line is in at most one block.
//!
//! A moved block annotates the diff and never changes it. It is an exact
//! copy, not a promise about the edit script: the script deletes and
//! inserts the lines the diff could not align, and some lines of a block
//! may be aligned elsewhere and print as context. In `P D E F` moved from
//! A to the end of B, with `D E F` also kept in place in B, only `P` is
//! deleted.
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
pub(crate) fn work(line_cap: Bytes) -> Bytes {
    let one = (READ_LINES * line_cap.0).max(PIECE.0) + READ_LINES * size_of::<usize>() as u64;
    Bytes(2 * 2 * one)
}
/// The block list takes at most this fraction, `1 / BLOCK_SHARE`, of the
/// refine half's capacity.
const BLOCK_SHARE: u64 = 8;
/// The block list grows by at least this many blocks at a time.
const BLOCK_STEP: usize = 16;

/// Moved blocks of a diff, ordered by their line in A, or the note that
/// the budget did not allow looking for them.
///
/// Memory, from the refine half: the block list's reservation,
/// [`list_bytes`](Self::list_bytes), taken by the caller and held for as
/// long as the list lives, and while detection runs two comparison buffers
/// of 16 lines, or of 64 KiB when that is more, doubled for growth:
/// 256 KiB for readers that return lines of up to 4 KiB whole. The list's
/// size depends on the budget and the seeds alone, so a caller that takes
/// it with detection off too leaves the same budget to what comes after.
/// When the buffers do not fit, or the list's reservation was refused,
/// detection keeps nothing and reports itself [`skipped`](Self::skipped).
/// When the list fills up, detection stops and keeps the blocks found so
/// far, reported as [`full`](Self::full).
#[derive(Debug)]
pub struct Moves {
    blocks: Vec<MovedBlock>,
    skipped: bool,
    full: bool,
    disabled: bool,
    _reservation: Option<Reservation>,
}

impl Moves {
    /// No search: an empty list that reports itself
    /// [`disabled`](Self::disabled). Reserves nothing.
    #[must_use]
    pub fn off() -> Self {
        Self::off_holding(None)
    }

    /// [`off`](Self::off) that holds `list`, the reservation the search
    /// would have used, so the budget left after it is the same either way.
    #[must_use]
    pub fn off_holding(list: Option<Reservation>) -> Self {
        Self {
            blocks: Vec::new(),
            skipped: false,
            full: false,
            disabled: true,
            _reservation: list,
        }
    }

    /// What the block list for `pairs` seeds may take under `budget`: 32
    /// bytes per block, at most one block per seed and at most `1 / 8` of
    /// the refine half's capacity. The caller reserves it from the refine
    /// half and hands it to [`find`](Self::find).
    #[must_use]
    pub fn list_bytes(pairs: usize, budget: &Budget) -> Bytes {
        let most = budget.capacity(Half::Refine).0 / BLOCK_SHARE / BLOCK_BYTES;
        Bytes((pairs as u64).min(most) * BLOCK_BYTES)
    }

    /// Finds the moved blocks seeded by `pairs`, `[line in A, line in B]`
    /// in A order, over the segment map `segments` of the files `a` and
    /// `b`, whose readers return lines of up to `line_cap` bytes whole.
    /// `list` holds [`list_bytes`](Self::list_bytes) of `pairs`; `None`, a
    /// refused reservation, skips the search.
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
        list: Option<Reservation>,
    ) -> Result<Self> {
        let Some(list) = list else {
            return Ok(Self::skipped_holding(None));
        };
        let Ok(_work) = budget.try_reserve(Half::Refine, work(line_cap)) else {
            return Ok(Self::skipped_holding(Some(list)));
        };
        let limit = usize::try_from(list.bytes().0 / BLOCK_BYTES).unwrap_or(usize::MAX);
        let mut run = Run {
            segments,
            a,
            b,
            buf_a: LineBuf::new(),
            buf_b: LineBuf::new(),
        };
        let mut blocks: Vec<MovedBlock> = Vec::new();
        let mut full = false;
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
                // `list` pays for `limit` blocks, the growth included.
                if extra == 0 || blocks.try_reserve_exact(extra).is_err() {
                    full = true;
                    break;
                }
            }
            blocks.push(MovedBlock {
                a: LineNo(x.0 - back)..x + ahead,
                b: LineNo(y.0 - back)..y + ahead,
            });
        }
        clip_b(&mut blocks);
        // The reservation keeps its size, so what is left of the half does
        // not depend on how many blocks were found.
        blocks.shrink_to_fit();
        Ok(Self {
            blocks,
            skipped: false,
            full,
            disabled: false,
            _reservation: Some(list),
        })
    }

    /// No search, because the budget did not allow one: an empty list that
    /// reports itself [`skipped`](Self::skipped). Reserves nothing.
    #[must_use]
    pub fn not_searched() -> Self {
        Self::skipped_holding(None)
    }

    fn skipped_holding(list: Option<Reservation>) -> Self {
        Self {
            blocks: Vec::new(),
            skipped: true,
            full: false,
            disabled: false,
            _reservation: list,
        }
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

    /// Whether the list filled up and detection stopped, so moves past the
    /// last block listed may be missing.
    #[must_use]
    pub fn full(&self) -> bool {
        self.full
    }

    /// Whether detection was switched off, so the empty list says nothing
    /// about the files.
    #[must_use]
    pub fn disabled(&self) -> bool {
        self.disabled
    }
}

/// Clips `blocks`, in A order, so that no line of B is in two of them. A
/// block's A range never overlaps another's, but a line of B can follow lines of A
/// from two places. In B order, a block that starts inside the B range of
/// one kept before it loses its first lines on both sides, which leaves an
/// exact copy, and is dropped when fewer than [`MIN_LINES`] remain. Sorts
/// in place, so it takes no heap.
fn clip_b(blocks: &mut Vec<MovedBlock>) {
    blocks.sort_unstable_by_key(|m| (m.b.start, m.a.start));
    let mut end = 0u64;
    blocks.retain_mut(|m| {
        let cut = end.saturating_sub(m.b.start.0).min(m.b.end.0 - m.b.start.0);
        m.a.start = LineNo(m.a.start.0 + cut);
        m.b.start = LineNo(m.b.start.0 + cut);
        let keep = m.b.end.0 - m.b.start.0 >= MIN_LINES;
        if keep {
            end = m.b.end.0;
        }
        keep
    });
    blocks.sort_unstable_by_key(|m| m.a.start);
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
