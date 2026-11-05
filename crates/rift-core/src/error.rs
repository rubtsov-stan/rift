//! The crate error type.

use std::io;

use crate::budget::BudgetExceeded;
use crate::types::{Bytes, LineNo};

/// Everything that can go wrong inside the core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The source failed to read.
    #[error("read failed: {0}")]
    Io(#[from] io::Error),
    /// A reservation did not fit its half of the budget.
    #[error(transparent)]
    Budget(#[from] BudgetExceeded),
    /// A line is longer than the buffer that must hold it whole.
    #[error("line {line} exceeds buffer of {cap} bytes")]
    LineTooLong {
        /// The offending line.
        line: LineNo,
        /// The buffer capacity.
        cap: Bytes,
    },
    /// A line number past the end of the file.
    #[error("line {line} out of range, file has {count} lines")]
    LineOutOfRange {
        /// The requested line.
        line: LineNo,
        /// Lines in the file.
        count: u64,
    },
    /// An edit script that does not satisfy the [`Hunk`](crate::Hunk)
    /// invariant.
    #[error("invalid edit script: {0}")]
    InvalidScript(String),
    /// Key mode could not compare the files.
    #[error(transparent)]
    Keyed(#[from] crate::keyed::KeyError),
}

/// Result alias with the crate error.
pub type Result<T, E = Error> = std::result::Result<T, E>;
