//! Unit newtypes shared by every module.
//!
//! Line numbers, byte offsets and byte counts are all `u64` underneath and
//! easy to mix up; the wrappers make the compiler catch it.

use std::fmt;
use std::ops::{Add, AddAssign, Sub};
use std::str::FromStr;

/// A 64-bit xxh3 hash of a full line, terminator included.
pub type LineHash = u64;

/// Zero-based line number within one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct LineNo(pub u64);

/// Byte position within one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ByteOffset(pub u64);

/// A byte count: a file length, a buffer size, a budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Bytes(pub u64);

macro_rules! arith {
    ($t:ident) => {
        impl Add<u64> for $t {
            type Output = $t;
            fn add(self, rhs: u64) -> $t {
                $t(self.0 + rhs)
            }
        }
        impl AddAssign<u64> for $t {
            fn add_assign(&mut self, rhs: u64) {
                self.0 += rhs;
            }
        }
        impl Sub for $t {
            type Output = u64;
            fn sub(self, rhs: $t) -> u64 {
                self.0 - rhs.0
            }
        }
        impl fmt::Display for $t {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

arith!(LineNo);
arith!(ByteOffset);
arith!(Bytes);

impl Bytes {
    /// A count in mebibytes.
    #[must_use]
    pub const fn mib(n: u64) -> Self {
        Bytes(n << 20)
    }

    /// The count as `usize`, for sizing allocations.
    ///
    /// # Panics
    ///
    /// Panics if the count does not fit a `usize`, which cannot happen on the
    /// 64-bit targets Rift supports.
    #[must_use]
    pub fn as_usize(self) -> usize {
        usize::try_from(self.0).expect("byte count fits usize on 64-bit targets")
    }

    /// Parses a size like [`FromStr`] and also rejects zero, for values that
    /// size a budget such as `--mem`.
    ///
    /// # Errors
    ///
    /// Any error of the [`FromStr`] impl, or [`ParseBytesError::Zero`].
    pub fn parse_budget(s: &str) -> Result<Self, ParseBytesError> {
        match s.parse()? {
            Bytes(0) => Err(ParseBytesError::Zero(s.to_owned())),
            b => Ok(b),
        }
    }
}

/// A size string that [`Bytes`] cannot parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseBytesError {
    /// Not digits followed by an optional unit.
    #[error(
        "invalid size {0:?}, expected digits with an optional K, M, G or T unit (binary, optional iB)"
    )]
    Invalid(String),
    /// The value does not fit in 64 bits.
    #[error("size {0:?} is too large")]
    Overflow(String),
    /// Zero where a budget is expected.
    #[error("size {0:?} must be above zero")]
    Zero(String),
}

/// Sizes in binary units: `4096`, `512K`, `256M`, `1G`, `2T`, and the same
/// with `iB` (`256MiB`). Units are case-insensitive. Surrounding whitespace
/// is ignored; whitespace inside, signs and fractions are rejected. Zero is
/// accepted here; use [`Bytes::parse_budget`] where it makes no sense.
impl FromStr for Bytes {
    type Err = ParseBytesError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || ParseBytesError::Invalid(s.to_owned());
        let t = s.trim();
        let digits_end = t.bytes().take_while(u8::is_ascii_digit).count();
        let (digits, unit) = t.split_at(digits_end);
        if digits.is_empty() {
            return Err(invalid());
        }
        let shift = match unit.to_ascii_uppercase().as_str() {
            "" => 0,
            "K" | "KIB" => 10,
            "M" | "MIB" => 20,
            "G" | "GIB" => 30,
            "T" | "TIB" => 40,
            _ => return Err(invalid()),
        };
        let overflow = || ParseBytesError::Overflow(s.to_owned());
        let n: u64 = digits.parse().map_err(|_| overflow())?;
        n.checked_mul(1 << shift).map(Bytes).ok_or_else(overflow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Bytes, ParseBytesError> {
        s.parse()
    }

    #[test]
    fn every_unit_is_binary_and_case_insensitive() {
        assert_eq!(parse("1000"), Ok(Bytes(1000)));
        for (unit, shift) in [("K", 10), ("M", 20), ("G", 30), ("T", 40)] {
            let want = Ok(Bytes(3 << shift));
            assert_eq!(parse(&format!("3{unit}")), want);
            assert_eq!(parse(&format!("3{}", unit.to_lowercase())), want);
            assert_eq!(parse(&format!("3{unit}iB")), want);
            assert_eq!(parse(&format!("3{}ib", unit.to_lowercase())), want);
        }
    }

    #[test]
    fn surrounding_whitespace_is_ignored_inner_is_not() {
        assert_eq!(parse("  256M\t"), Ok(Bytes::mib(256)));
        assert!(matches!(parse("256 M"), Err(ParseBytesError::Invalid(_))));
        assert!(matches!(parse("2 56M"), Err(ParseBytesError::Invalid(_))));
        assert!(matches!(parse("   "), Err(ParseBytesError::Invalid(_))));
    }

    #[test]
    fn malformed_sizes_are_invalid() {
        for s in [
            "", "M", "iB", "1.5G", "+1M", "-1M", "1B", "1KB", "1Mi", "0x10", "1MiBB",
        ] {
            assert!(
                matches!(parse(s), Err(ParseBytesError::Invalid(_))),
                "{s:?}"
            );
        }
    }

    #[test]
    fn overflow_is_reported_not_wrapped() {
        assert_eq!(parse("18446744073709551615"), Ok(Bytes(u64::MAX)));
        assert!(matches!(
            parse("18446744073709551616"),
            Err(ParseBytesError::Overflow(_))
        ));
        assert!(matches!(
            parse("16777216T"),
            Err(ParseBytesError::Overflow(_))
        ));
        assert_eq!(parse("16777215T"), Ok(Bytes(16_777_215 << 40)));
    }

    #[test]
    fn zero_is_a_size_but_not_a_budget() {
        assert_eq!(parse("0"), Ok(Bytes(0)));
        assert_eq!(parse("0M"), Ok(Bytes(0)));
        assert!(matches!(
            Bytes::parse_budget("0G"),
            Err(ParseBytesError::Zero(_))
        ));
        assert_eq!(Bytes::parse_budget("256M"), Ok(Bytes::mib(256)));
        assert!(matches!(
            Bytes::parse_budget("x"),
            Err(ParseBytesError::Invalid(_))
        ));
    }

    #[test]
    fn arithmetic_keeps_units() {
        let mut l = LineNo(5);
        l += 2;
        assert_eq!(l + 1, LineNo(8));
        assert_eq!(LineNo(8) - LineNo(5), 3);
        assert_eq!(Bytes::mib(2), Bytes(2 << 20));
        assert_eq!(format!("{}", ByteOffset(42)), "42");
    }
}
