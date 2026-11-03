//! Stratum selection and in-place thinning of sample records.
//!
//! A line belongs to stratum `L` when the low `L` bits of its hash are zero,
//! so stratum `L + 1` is a subset of stratum `L` with half the lines on
//! average, and membership depends on the line's content alone. The starting
//! level comes from an estimated line count and the sample capacity; when
//! the estimate is wrong the scanner raises the level in place, without
//! rereading, through [`thin`].

use crate::sample::Sample;
use crate::types::LineHash;

/// The highest stratum. Above it the mask would cover more than the hash.
pub const MAX_LEVEL: u32 = 63;

/// Bit mask that must be zero for a hash to belong to stratum `level`.
///
/// # Panics
///
/// Panics if `level > MAX_LEVEL`.
#[must_use]
pub fn mask(level: u32) -> u64 {
    assert!(level <= MAX_LEVEL, "stratum {level} above {MAX_LEVEL}");
    (1u64 << level) - 1
}

/// Whether a line with this hash belongs to stratum `level`.
#[inline]
#[must_use]
pub fn contains(level: u32, hash: LineHash) -> bool {
    hash & mask(level) == 0
}

/// Keeps only the samples of stratum `level`, in place and in order.
pub fn thin(samples: &mut Vec<Sample>, level: u32) {
    let mask = mask(level);
    samples.retain(|s| s.hash & mask == 0);
}

/// The lowest stratum at which `lines` lines yield at most `cap` samples
/// on average: the smallest `L` with `lines >> L <= cap`.
#[must_use]
pub fn initial_level(lines: u64, cap: u64) -> u32 {
    (0..MAX_LEVEL)
        .find(|&level| (lines >> level) <= cap)
        .unwrap_or(MAX_LEVEL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ByteOffset, LineNo};

    fn sample(hash: u64) -> Sample {
        Sample {
            hash,
            line: LineNo(0),
            offset: ByteOffset(0),
        }
    }

    #[test]
    fn membership_is_the_low_bits() {
        assert!(contains(0, 0xffff));
        assert!(contains(3, 0b1000));
        assert!(!contains(4, 0b1000));
        assert!(contains(MAX_LEVEL, 1 << 63));
        assert!(!contains(MAX_LEVEL, 1));
    }

    #[test]
    fn thinning_keeps_order_and_nests() {
        let mut v: Vec<Sample> = (0..64u64).map(sample).collect();
        thin(&mut v, 2);
        let at2: Vec<u64> = v.iter().map(|s| s.hash).collect();
        assert_eq!(at2, (0..64).step_by(4).collect::<Vec<_>>());
        thin(&mut v, 3);
        assert!(v.iter().all(|s| at2.contains(&s.hash)));
        assert_eq!(v.len(), 8);
    }

    #[test]
    fn level_halves_the_lines_until_they_fit() {
        assert_eq!(initial_level(100, 1000), 0);
        assert_eq!(initial_level(1000, 100), 4);
        assert_eq!(initial_level(335_544_321, 3_145_728), 7);
        assert_eq!(initial_level(u64::MAX, 0), MAX_LEVEL);
    }
}
