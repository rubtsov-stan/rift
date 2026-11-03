//! `SplitMix64`: small, fast and deterministic; good enough for test data.

/// A 64-bit `SplitMix` generator.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// Creates a generator seeded with `seed`.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    /// Next 64 random bits.
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform value in `0..n`. Returns 0 when `n` is 0.
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        // Lemire's multiply-shift; the bias is below 2^-64, irrelevant here.
        ((u128::from(self.next_u64()) * u128::from(n)) >> 64) as u64
    }

    /// Uniform index in `0..n`.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "result is below n, a usize"
    )]
    pub fn index(&mut self, n: usize) -> usize {
        self.below(n as u64) as usize
    }

    /// Uniform byte in `0..n`.
    #[allow(clippy::cast_possible_truncation, reason = "result is below n, a u8")]
    pub fn byte(&mut self, n: u8) -> u8 {
        self.below(u64::from(n)) as u8
    }

    /// Uniform value in `lo..=hi`.
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi - lo + 1)
    }

    /// True with probability `p`.
    #[allow(
        clippy::cast_precision_loss,
        reason = "53 bits fit an f64 mantissa exactly"
    )]
    pub fn chance(&mut self, p: f64) -> bool {
        // 53 random bits give a uniform double in [0, 1).
        let unit = (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64;
        unit < p
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn below_stays_in_range() {
        let mut r = Rng::new(7);
        for _ in 0..10_000 {
            assert!(r.below(13) < 13);
            let v = r.range(5, 9);
            assert!((5..=9).contains(&v));
        }
        assert_eq!(r.below(0), 0);
    }
}
