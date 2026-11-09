//! What a session call can refuse, with the protocol's error code for each.

use std::fmt;

use rift_core::{BudgetExceeded, Bytes, LineNo};

use crate::file::FileError;
use crate::{SessionId, SessionState};

/// JSON-RPC error codes of the protocol in `docs/protocol.md`.
///
/// The JSON-RPC ones and the two taken from LSP are listed so the table
/// lives in one place; only the server's reader produces them.
pub mod code {
    /// The body is not JSON.
    pub const PARSE_ERROR: i32 = -32700;
    /// Not a JSON-RPC request, or a batch.
    pub const INVALID_REQUEST: i32 = -32600;
    /// Unknown method.
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// Params that do not match the method, or a number out of range.
    pub const INVALID_PARAMS: i32 = -32602;
    /// A bug.
    pub const INTERNAL_ERROR: i32 = -32603;
    /// A request before `initialize`; LSP's code.
    pub const NOT_INITIALIZED: i32 = -32002;
    /// The client cancelled the request; LSP's code.
    pub const CANCELLED: i32 = -32800;
    /// A path cannot be opened for the diff.
    pub const FILE: i32 = -32001;
    /// The diff does not fit its memory budget.
    pub const BUDGET_EXCEEDED: i32 = -32003;
    /// A budget over the server's limit for one session.
    pub const TOO_LARGE: i32 = -32004;
    /// No room for another session, or for another response.
    pub const BUSY: i32 = -32005;
    /// The session was closed, dropped, or never existed.
    pub const UNKNOWN_SESSION: i32 = -32006;
    /// A file failed to read after it was opened.
    pub const IO: i32 = -32007;
    /// The session is scanning or its scan failed.
    pub const NOT_READY: i32 = -32008;
}

/// Which input a file error is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// File A, the old one.
    A,
    /// File B, the new one.
    B,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::A => "a",
            Self::B => "b",
        })
    }
}

/// Everything a session call can refuse.
///
/// The messages are the ones `rift-mcp` has always given its clients.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A path cannot be opened for the diff.
    #[error("{side}: {error}")]
    File {
        /// The input.
        side: Side,
        /// Why.
        error: FileError,
    },
    /// The budget asked for is over the limit for one session.
    #[error("mem {mem} is over the limit of {limit} per diff")]
    MemOverLimit {
        /// The budget asked for.
        mem: Bytes,
        /// The limit.
        limit: Bytes,
    },
    /// Every session slot, or all the memory for sessions, is held by
    /// sessions in use.
    #[error("the server is busy with other diffs; retry later")]
    Busy,
    /// Other responses of the session hold its response pool.
    #[error("the session is busy with other responses; retry later")]
    ResponsesBusy,
    /// No live session has this id.
    #[error("no session {0}; it was closed or dropped, open the files again")]
    UnknownSession(SessionId),
    /// The session has no map: it is scanning, or its scan failed.
    #[error("{}", not_ready(*session, *state))]
    NotReady {
        /// The session.
        session: SessionId,
        /// Its state, never [`SessionState::Ready`].
        state: SessionState,
    },
    /// A segment index past the end of the map.
    #[error("segment {index} is out of range, the diff has {count} segments")]
    SegmentOutOfRange {
        /// The index asked for.
        index: usize,
        /// Segments in the map.
        count: usize,
    },
    /// A line of A past its end.
    #[error("line {} is out of range, a has {count} lines", line.0 + 1)]
    LineOutOfRange {
        /// The line asked for, 0-based.
        line: LineNo,
        /// Lines in A.
        count: u64,
    },
    /// Scanning or building the map failed.
    #[error("cannot diff within {mem}: {error}")]
    Build {
        /// The session's budget.
        mem: Bytes,
        /// The core's error.
        error: rift_core::Error,
    },
    /// Refining or reading one segment failed.
    #[error("segment {segment}: {error}")]
    Segment {
        /// The segment.
        segment: usize,
        /// The session's budget.
        mem: Bytes,
        /// The core's error.
        error: rift_core::Error,
    },
}

impl Error {
    /// The protocol's error code for this error.
    #[must_use]
    pub fn code(&self) -> i32 {
        match self {
            Self::File { .. } => code::FILE,
            Self::MemOverLimit { .. } => code::TOO_LARGE,
            Self::Busy | Self::ResponsesBusy => code::BUSY,
            Self::UnknownSession(_) => code::UNKNOWN_SESSION,
            Self::NotReady { .. } => code::NOT_READY,
            Self::SegmentOutOfRange { .. } | Self::LineOutOfRange { .. } => code::INVALID_PARAMS,
            Self::Build { error, .. } | Self::Segment { error, .. } => match error {
                rift_core::Error::Budget(_) => code::BUDGET_EXCEEDED,
                rift_core::Error::Io(_) => code::IO,
                _ => code::INTERNAL_ERROR,
            },
        }
    }

    /// The core's error behind this one, if any.
    #[must_use]
    pub fn core(&self) -> Option<&rift_core::Error> {
        match self {
            Self::Build { error, .. } | Self::Segment { error, .. } => Some(error),
            _ => None,
        }
    }

    /// For a budget refusal: the session's budget and the refusal.
    #[must_use]
    pub fn budget(&self) -> Option<(Bytes, BudgetExceeded)> {
        match self {
            Self::Build {
                mem,
                error: rift_core::Error::Budget(b),
            }
            | Self::Segment {
                mem,
                error: rift_core::Error::Budget(b),
                ..
            } => Some((*mem, *b)),
            _ => None,
        }
    }

    /// For a budget refusal, a budget to open the files again with: the
    /// value `rift` prints as `rerun with --mem`. See
    /// [`BudgetExceeded::mem_advice`].
    #[must_use]
    pub fn suggested_mem(&self) -> Option<Bytes> {
        self.budget().and_then(|(mem, b)| b.mem_advice(mem))
    }
}

/// The message of [`Error::NotReady`]: a scan in progress is waited for,
/// a failed one is opened again.
fn not_ready(session: SessionId, state: SessionState) -> String {
    match state {
        SessionState::Failed => format!("session {session} failed; open the files again"),
        _ => format!(
            "session {session} is {}; wait for it to be ready",
            state.name()
        ),
    }
}
