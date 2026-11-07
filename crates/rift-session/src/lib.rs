//! Live Rift diffs of file pairs: the session logic shared by `rift serve`
//! and `rift-mcp`.
//!
//! A [`Sessions`] keeps one [`Session`] per pair of files, opened by path
//! with the checks in [`mod@file`], bounded in count and memory by
//! [`Limits`], and dropped when idle or when a new pair needs room. A
//! session opened with [`Sessions::open`] scans on a thread of its own and
//! reports its [`SessionState`] and [`Session::scan_progress`] meanwhile;
//! [`Sessions::close`] stops that scan. A ready session answers what the
//! protocol in `docs/protocol.md` asks: [`Session::summary`],
//! [`Session::differing`], [`Session::hunk_state`] and
//! [`Session::excerpt`]; the rest (`set_visible`, progress, moved blocks)
//! goes straight to [`Session::diff`]. Errors are typed, and
//! [`Error::code`] gives each its protocol code. Wire formats are not
//! here: the server and `rift-mcp` own their JSON.
//!
//! # Synchronous
//!
//! Every call blocks the calling thread, except that [`Sessions::open`]
//! leaves the scan to a thread it starts, and is safe to make from many
//! threads at once; there is no async runtime. The core works with threads
//! and `pread`, and has nothing to await: a scan is two passes over the
//! disk, a refinement is CPU work on the scheduler's own threads, and
//! [`Session::excerpt`] is a bounded number of positional reads. An async
//! API would only wrap those in `spawn_blocking`, and would bring a
//! runtime into `rift serve`, which needs none: it reads requests on one
//! thread and answers each on a thread of its own. `rift-mcp`, whose MCP
//! library runs on tokio, calls in from `spawn_blocking`.
//!
//! # Memory
//!
//! Every session has its own budget `mem`, split into the two halves of
//! the invariant in `DESIGN.md`, and [`Limits`] bounds the sum of all of
//! them. What each allocation path is paid from:
//!
//! - the diff (skeleton, index, segment map, workers, hunk cache, moved
//!   blocks): both halves, as in `rift-core`;
//! - the response pool of a session from [`Sessions::open`]: the Refine
//!   half, taken before the diff is built, 1/[`RESPONSE_SHARE`] of the
//!   half and at most [`MAX_RESPONSE_POOL`];
//! - an [`Excerpt`]: its lines, their text and one line buffer, inside the
//!   response pool (the free Refine half for a session from
//!   [`Sessions::get`]), held until the excerpt is dropped;
//! - [`Summary`] and pages of [`Session::differing`] and moved blocks: no
//!   heap; they borrow from the map and the block list;
//! - the session table and each session's scan thread: a few hundred
//!   bytes per session, outside any budget, bounded by
//!   [`Limits::sessions`] plus the closed sessions whose scan has not yet
//!   reached its next read.
#![forbid(unsafe_code)]

mod error;
pub mod file;
mod session;
mod sessions;

pub use error::{Error, Side, code};
pub use session::{
    At, Column, DEFAULT_CONTEXT, DEFAULT_LINE_BYTES, DEFAULT_LINES, DEFAULT_PAGE, Eol, Excerpt,
    ExcerptRequest, Line, MAX_CONTEXT, MAX_LINE_BYTES, MAX_LINES, MAX_PAGE, MovesState, Summary,
    Tally,
};
pub use sessions::{
    DEFAULT_MEM, IDLE_TIMEOUT, Limits, MAX_RESPONSE_POOL, MAX_SESSION_MEM, MAX_SESSIONS,
    MAX_TOTAL_MEM, Opened, RESPONSE_SHARE, Session, SessionId, SessionState, Sessions,
};
