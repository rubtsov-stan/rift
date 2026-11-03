//! Unit newtypes shared by every module.
//!
//! Line numbers, byte offsets and byte counts are all `u64` underneath and
//! easy to mix up; the wrappers make the compiler catch it.

use std::fmt;
use std::ops::{Add, AddAssign, Sub};

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
}

#[cfg(test)]
mod tests {
    use super::*;

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
