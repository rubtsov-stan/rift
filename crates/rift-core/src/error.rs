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
    /// The output failed to write.
    ///
    /// Apart from [`Io`](Self::Io) so a caller can tell a closed pipe on
    /// the output from a failing source.
    #[error("write failed: {0}")]
    Write(io::Error),
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

impl Error {
    /// The output was closed by its reader, as in `rift a b | head`.
    #[must_use]
    pub fn is_broken_pipe(&self) -> bool {
        matches!(self, Self::Write(e) if e.kind() == io::ErrorKind::BrokenPipe)
    }
}

/// Result alias with the crate error.
pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use super::*;
    use crate::edit::{Edit, Hunk};
    use crate::lines::MemLines;
    use crate::script::apply;
    use crate::unified::{Label, Options, write_diff};

    /// A writer whose reader is gone.
    struct Closed;

    impl Write for Closed {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }

    fn changed() -> Vec<Hunk> {
        vec![Hunk {
            edits: vec![
                Edit::Delete {
                    a: LineNo(0),
                    len: 1,
                },
                Edit::Insert {
                    b: LineNo(0),
                    len: 1,
                },
            ],
            approx: false,
        }]
    }

    #[test]
    fn a_failed_write_says_write_and_a_failed_read_says_read() {
        let w = Error::Write(io::ErrorKind::BrokenPipe.into());
        assert_eq!(w.to_string(), "write failed: broken pipe");
        let r = Error::from(io::Error::from(io::ErrorKind::BrokenPipe));
        assert_eq!(r.to_string(), "read failed: broken pipe");
    }

    #[test]
    fn only_a_write_to_a_closed_pipe_is_a_broken_pipe() {
        assert!(Error::Write(io::ErrorKind::BrokenPipe.into()).is_broken_pipe());
        assert!(!Error::Write(io::ErrorKind::PermissionDenied.into()).is_broken_pipe());
        assert!(!Error::Io(io::ErrorKind::BrokenPipe.into()).is_broken_pipe());
    }

    #[test]
    fn a_unified_diff_to_a_closed_writer_is_a_write_error() {
        let hunks = changed();
        let opts = Options {
            context: 3,
            old: Label {
                name: b"a",
                mtime: None,
            },
            new: Label {
                name: b"b",
                mtime: None,
            },
        };
        let err = write_diff(
            &MemLines::new(b"x\n"),
            &MemLines::new(b"y\n"),
            |i| Ok(hunks.get(i)),
            &opts,
            Closed,
        )
        .unwrap_err();
        assert!(err.is_broken_pipe(), "{err}");
    }

    #[test]
    fn applying_a_script_to_a_closed_writer_is_a_write_error() {
        let hunks = changed();
        let err = apply(
            &MemLines::new(b"x\n"),
            &MemLines::new(b"y\n"),
            &hunks,
            &mut Closed,
        )
        .unwrap_err();
        assert!(err.is_broken_pipe(), "{err}");
    }
}
