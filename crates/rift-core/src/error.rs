//! The crate error type.

use std::io;

use crate::budget::BudgetExceeded;
use crate::types::{Bytes, LineNo};

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Everything that can go wrong inside the core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The source failed to read.
    ///
    /// Not a `#[from]` field: that would make the I/O error the source as
    /// well, and a chain printer such as `anyhow`'s would show its text twice.
    #[error("read failed: {0}")]
    Io(io::Error),
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
    /// The scan was stopped through its [`Watch`](crate::scan::Watch).
    #[error("the scan was cancelled")]
    Cancelled,
}

/// Result alias with the crate error.
pub type Result<T, E = Error> = std::result::Result<T, E>;
