//! Shingle anchors: windows of consecutive samples that occur once in each
//! file.
//!
//! The samples are content-defined, so an equal stretch of the two files
//! yields the same run of samples on both sides and the same windows. A
//! window's hash combines the line hashes of its `w` samples with the line
//! count and byte length between each sample and the next, so two windows
//! match only when their sampled lines match and so do the sizes of what
//! lies between them. At stratum 0, where every line is a sample, a window
//! is `w` consecutive lines. Nothing reads the files: the hash is a
//! polynomial over the line hashes, rolled from one window to the next.

use std::ops::Range;

use super::{Anchors, CandidateSource, ENTRY_BYTES, MANY, unique_hashes};
use crate::budget::{Budget, BudgetExceeded, Half};
use crate::error::Result;
use crate::sample::Sample;
use crate::types::Bytes;

/// Windows of `w` consecutive samples whose combined hash occurs exactly
/// once among the windows of each file. A candidate points at the first
/// sample of its window. Time and memory as for
/// [`UniqueLines`](super::UniqueLines), over windows instead of samples.
#[derive(Debug, Clone, Copy)]
pub struct Shingles {
    /// Samples per window, at least 2.
    pub w: usize,
}

impl CandidateSource for Shingles {
    fn select(
        &self,
        samples_a: &[Sample],
        samples_b: &[Sample],
        budget: &Budget,
    ) -> Result<Anchors> {
        let (whole_a, whole_b) = (0..samples_a.len(), 0..samples_b.len());
        self.in_gaps(
            (samples_a, std::slice::from_ref(&whole_a)),
            (samples_b, std::slice::from_ref(&whole_b)),
            budget,
        )
    }
}

impl Shingles {
    /// Candidates among the windows that lie inside the given ranges of
    /// sample indices, the gaps. A window counts as unique if no other
    /// window of the gaps has its hash.
    pub(super) fn in_gaps(
        self,
        (samples_a, gaps_a): (&[Sample], &[Range<usize>]),
        (samples_b, gaps_b): (&[Sample], &[Range<usize>]),
        budget: &Budget,
    ) -> Result<Anchors> {
        assert!(self.w >= 2, "a shingle spans at least two samples");
        let most = samples_a.len().max(samples_b.len()) as u64;
        if most >= u64::from(MANY) {
            return Err(BudgetExceeded {
                half: Half::Skeleton,
                requested: Bytes(most * ENTRY_BYTES),
                available: budget.available(Half::Skeleton),
            }
            .into());
        }
        unique_hashes(
            gap_windows(samples_a, gaps_a, self.w),
            gap_windows(samples_b, gaps_b, self.w),
            budget,
        )
    }
}

/// The window count and `(first sample, hash)` of each window of `w`
/// samples inside `gaps`, which hold at least `w` samples each.
fn gap_windows<'a>(
    samples: &'a [Sample],
    gaps: &'a [Range<usize>],
    w: usize,
) -> (usize, impl Iterator<Item = (usize, u64)> + 'a) {
    let count = gaps.iter().map(|r| (r.len() + 1).saturating_sub(w)).sum();
    let windows = gaps.iter().flat_map(move |r| {
        let start = r.start;
        windows(&samples[r.clone()], w)
            .enumerate()
            .map(move |(k, hash)| (start + k, hash))
    });
    (count, windows)
}

/// Odd multiplier of the rolling polynomial.
const BASE: u64 = 0x9e37_79b9_7f4a_7c15;

/// The hash of each window, in order.
fn windows(samples: &[Sample], w: usize) -> Windows<'_> {
    let count = (samples.len() + 1).saturating_sub(w);
    let top = (0..w - 2).fold(1u64, |p, _| p.wrapping_mul(BASE));
    let mut it = Windows {
        samples,
        w,
        top,
        start: 0,
        count,
        rolled: 0,
    };
    for k in 0..(w - 1).min(samples.len().saturating_sub(1)) {
        it.rolled = it.rolled.wrapping_mul(BASE).wrapping_add(it.step(k));
    }
    it
}

/// Rolls `rolled`, the polynomial over the steps of samples `start ..
/// start + w - 1`, along the samples.
struct Windows<'a> {
    samples: &'a [Sample],
    w: usize,
    /// `BASE^(w - 2)`, the weight of the oldest step.
    top: u64,
    start: usize,
    count: usize,
    rolled: u64,
}

impl Windows<'_> {
    /// Sample `k` and the distance from it to sample `k + 1`.
    fn step(&self, k: usize) -> u64 {
        let (s, next) = (&self.samples[k], &self.samples[k + 1]);
        let lines = next.line.0.wrapping_sub(s.line.0);
        let bytes = next.offset.0.wrapping_sub(s.offset.0);
        mix(s.hash ^ mix(lines ^ mix(bytes)))
    }
}

impl Iterator for Windows<'_> {
    type Item = u64;

    fn next(&mut self) -> Option<u64> {
        if self.start == self.count {
            return None;
        }
        let i = self.start;
        let last = self.samples[i + self.w - 1].hash;
        let hash = mix(self.rolled.wrapping_mul(BASE).wrapping_add(mix(last)));
        self.start += 1;
        if self.start < self.count {
            self.rolled = self
                .rolled
                .wrapping_sub(self.step(i).wrapping_mul(self.top))
                .wrapping_mul(BASE)
                .wrapping_add(self.step(i + self.w - 1));
        }
        Some(hash)
    }
}

/// `splitmix64`'s finalizer: line hashes have the stratum's low bits zero
/// and the distances are small integers, and the table takes its slot from
/// the high bits.
fn mix(mut x: u64) -> u64 {
    x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anchors::Pair;
    use crate::types::{ByteOffset, Bytes, LineNo};

    /// One sample per line, each line two bytes long.
    fn samples(hashes: &[u64]) -> Vec<Sample> {
        (0u64..)
            .zip(hashes)
            .map(|(i, &hash)| Sample {
                hash,
                line: LineNo(i),
                offset: ByteOffset(2 * i),
            })
            .collect()
    }

    /// Each window hashed from scratch, to check the rolling update.
    fn direct(samples: &[Sample], w: usize) -> Vec<u64> {
        let it = windows(samples, w);
        (0..it.count)
            .map(|i| {
                let rolled =
                    (i..i + w - 1).fold(0u64, |r, k| r.wrapping_mul(BASE).wrapping_add(it.step(k)));
                mix(rolled
                    .wrapping_mul(BASE)
                    .wrapping_add(mix(samples[i + w - 1].hash)))
            })
            .collect()
    }

    #[test]
    fn rolling_matches_hashing_each_window() {
        let s = samples(&[5, 1, 5, 2, 5, 1, 5, 3, 9, 9, 9]);
        for w in 2..=s.len() + 1 {
            let rolled: Vec<u64> = windows(&s, w).collect();
            assert_eq!(rolled.len(), (s.len() + 1).saturating_sub(w), "w={w}");
            assert_eq!(rolled, direct(&s, w), "w={w}");
        }
    }

    #[test]
    fn a_window_unique_in_both_files_is_a_candidate() {
        // No line occurs once in both files, but four windows do.
        let a = samples(&[1, 1, 2, 1, 3, 3]);
        let b = samples(&[3, 1, 2, 1, 1, 3]);
        let budget = Budget::new(Bytes::mib(1));
        let got = Shingles { w: 2 }.select(&a, &b, &budget).unwrap();
        assert_eq!(
            got.pairs(),
            [
                Pair { a: 0, b: 3 },
                Pair { a: 1, b: 1 },
                Pair { a: 2, b: 2 },
                Pair { a: 3, b: 4 }
            ]
        );
    }

    #[test]
    fn the_distance_between_samples_is_part_of_the_window() {
        let a = samples(&[1, 2]);
        let mut b = samples(&[1, 2]);
        b[1].line = LineNo(2);
        b[1].offset = ByteOffset(4);
        let budget = Budget::new(Bytes::mib(1));
        let got = Shingles { w: 2 }.select(&a, &b, &budget).unwrap();
        assert_eq!(got.pairs(), []);
    }
}
