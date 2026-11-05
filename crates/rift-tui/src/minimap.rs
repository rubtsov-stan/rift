//! Density of Differs and Approx across the whole file, in `width` cells.

use rift_core::{Segment, SegmentStatus};

/// Per-cell counts, weighted by `max(len_a, len_b)` of each segment.
#[derive(Debug, Default)]
pub(crate) struct Minimap {
    width: usize,
    differs: Vec<u64>,
    approx: Vec<u64>,
}

/// Weight of a segment in the minimap: the longer of its two sides.
pub(crate) fn weight(s: &Segment) -> u64 {
    s.len_a().max(s.len_b())
}

/// Whether the segment is a difference.
pub(crate) fn is_diff(s: &Segment) -> u64 {
    u64::from(s.status != SegmentStatus::Equal)
}

/// Running sum of `f` over `segments[..seg]`, kept at the last asked
/// position. Moving costs the distance moved, so a frame near the previous
/// one is cheap and no per-segment array is needed.
#[derive(Debug, Default)]
pub(crate) struct Cursor {
    seg: usize,
    sum: u64,
}

impl Cursor {
    pub(crate) fn sum_to(
        &mut self,
        segments: &[Segment],
        seg: usize,
        f: fn(&Segment) -> u64,
    ) -> u64 {
        while self.seg < seg {
            self.sum += f(&segments[self.seg]);
            self.seg += 1;
        }
        while self.seg > seg {
            self.seg -= 1;
            self.sum -= f(&segments[self.seg]);
        }
        self.sum
    }
}

fn cell_of(pos: u64, total: u64, width: usize) -> usize {
    let c = u128::from(pos) * width as u128 / u128::from(total);
    usize::try_from(c).unwrap_or(width - 1).min(width - 1)
}

/// First weighted position of cell `c`.
fn cell_start(c: usize, total: u64, width: usize) -> u64 {
    let s = (c as u128 * u128::from(total)).div_ceil(width as u128);
    u64::try_from(s).expect("at most total")
}

impl Minimap {
    pub(crate) fn width(&self) -> usize {
        self.width
    }

    /// Rebuilds the cells; reuses their allocation. O(m + width).
    pub(crate) fn rebuild(&mut self, segments: &[Segment], total: u64, width: usize) {
        self.width = width;
        self.differs.clear();
        self.approx.clear();
        self.differs.resize(width, 0);
        self.approx.resize(width, 0);
        if width == 0 || total == 0 {
            return;
        }
        let mut at = 0;
        for s in segments {
            let (lo, hi) = (at, at + weight(s));
            at = hi;
            let acc = match s.status {
                SegmentStatus::Equal => continue,
                SegmentStatus::Differs => &mut self.differs,
                SegmentStatus::Approx => &mut self.approx,
            };
            let (first, last) = (cell_of(lo, total, width), cell_of(hi - 1, total, width));
            for (c, cell) in acc.iter_mut().enumerate().take(last + 1).skip(first) {
                let from = lo.max(cell_start(c, total, width));
                let to = hi.min(cell_start(c + 1, total, width).max(from));
                *cell += to - from;
            }
        }
    }

    /// Density level 0..=8 of cell `c` in the given row.
    pub(crate) fn level(&self, approx: bool, c: usize, total: u64) -> usize {
        let v = if approx { &self.approx } else { &self.differs }[c];
        let span = cell_start(c + 1, total, self.width) - cell_start(c, total, self.width);
        if v == 0 || span == 0 {
            return 0;
        }
        let l = (u128::from(v) * 8).div_ceil(u128::from(span));
        usize::try_from(l).unwrap_or(8).clamp(1, 8)
    }

    /// Cell holding weighted position `pos`.
    pub(crate) fn cell(&self, pos: u64, total: u64) -> usize {
        cell_of(
            pos.min(total.saturating_sub(1)),
            total.max(1),
            self.width.max(1),
        )
    }
}
