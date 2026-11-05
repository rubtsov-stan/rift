//! Density of Differs and Approx across the whole file, in `width` cells.

use rift_core::{Segment, SegmentStatus};

/// Per-cell counts, weighted by `max(len_a, len_b)` of each segment.
#[derive(Debug, Default)]
pub(crate) struct Minimap {
    width: usize,
    differs: Vec<u64>,
    approx: Vec<u64>,
}

/// Start of each segment in the weighted space, plus the total at the end.
pub(crate) fn starts(segments: &[Segment]) -> Vec<u64> {
    let mut v = Vec::with_capacity(segments.len() + 1);
    let mut at = 0u64;
    v.push(0);
    for s in segments {
        at += s.len_a().max(s.len_b());
        v.push(at);
    }
    v
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
    pub(crate) fn rebuild(&mut self, segments: &[Segment], starts: &[u64], width: usize) {
        self.width = width;
        self.differs.clear();
        self.approx.clear();
        self.differs.resize(width, 0);
        self.approx.resize(width, 0);
        let total = starts.last().copied().unwrap_or(0);
        if width == 0 || total == 0 {
            return;
        }
        for (s, w) in segments.iter().zip(starts.windows(2)) {
            let acc = match s.status {
                SegmentStatus::Equal => continue,
                SegmentStatus::Differs => &mut self.differs,
                SegmentStatus::Approx => &mut self.approx,
            };
            let (lo, hi) = (w[0], w[1]);
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
