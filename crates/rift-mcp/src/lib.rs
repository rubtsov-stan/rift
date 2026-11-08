//! MCP server that lets a client explore a Rift diff of two large files
//! through small, bounded answers.
//!
//! Tools: `summary`, `hunks_at` and `next_difference`. Line numbers in
//! requests and answers are 1-based lines of file A (or B where named). A
//! range is `start` and `count`; an empty range (`count` 0) sits before line
//! `start`. A range comes from one segment of the diff, which may be shorter
//! than the hunk `diff -u` would print for the same lines.
//!
//! Bounds on one answer: [`render::MAX_TEXT_BYTES`] bytes of diff text,
//! [`render::MAX_LINES`] diff lines, [`render::MAX_LINE_BYTES`] bytes per line
//! and [`SUMMARY_RANGES`] ranges. Memory bounds are in [`sessions`], which
//! is the `rift-session` crate the server keeps its diffs in.
#![forbid(unsafe_code)]

pub mod render;

pub use rift_session as sessions;
pub use rift_session::file;

use std::sync::Arc;

use rift_core::{Bytes, Segment, SegmentStatus};
use rift_session::Session;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, ServerCapabilities, ServerConfig};
use rmcp::{Json, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use file::FileSource;
use sessions::{Limits, Sessions};

/// Differing ranges listed by `summary`; the rest are only counted.
pub const SUMMARY_RANGES: usize = 20;

/// Which two files to compare.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct Pair {
    /// Path of file A, the old one. Must be a regular file, not a symlink.
    pub a: String,
    /// Path of file B, the new one. Must be a regular file, not a symlink.
    pub b: String,
    /// Memory budget for the diff of this pair, like `--mem`: `64M`, `1G`.
    /// Used only when the call builds the diff; a live diff keeps its own.
    /// Default 64M, at most 256M.
    pub mem: Option<String>,
}

/// Arguments of `hunks_at`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct HunksAt {
    /// The files.
    #[serde(flatten)]
    pub pair: Pair,
    /// 1-based line of A.
    pub line: u64,
    /// Equal lines to show around the change, default 3, at most 20.
    pub context: Option<u64>,
    /// Most diff lines to return, default 100, at most 500.
    pub max_lines: Option<u64>,
}

/// Arguments of `next_difference`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct NextDifference {
    /// The files.
    #[serde(flatten)]
    pub pair: Pair,
    /// The answer starts after this 1-based line of A; 0 for the first
    /// difference. Pass a returned `a.start` to get the one after it.
    pub after_line: u64,
}

/// A differing range.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Range {
    /// Lines of A.
    pub a: Lines,
    /// Lines of B.
    pub b: Lines,
    /// `differs`, or `approx` when the local diff was cut short.
    pub status: String,
}

/// A run of lines.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Lines {
    /// First line, 1-based.
    pub start: u64,
    /// Number of lines.
    pub count: u64,
}

/// Segments of one status.
#[derive(Debug, Default, Serialize, JsonSchema)]
pub struct Tally {
    /// Segments.
    pub segments: u64,
    /// Their lines in A.
    pub a_lines: u64,
    /// Their lines in B.
    pub b_lines: u64,
}

/// Answer of `summary`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Summary {
    /// Lines in A.
    pub a_lines: u64,
    /// Lines in B.
    pub b_lines: u64,
    /// Bytes in A.
    pub a_bytes: u64,
    /// Bytes in B.
    pub b_bytes: u64,
    /// The files hold the same bytes.
    pub identical: bool,
    /// Equal segments.
    pub equal: Tally,
    /// Differing segments not known to be approximate: not refined yet, or
    /// refined exactly.
    pub differs: Tally,
    /// Refined segments whose local diff was cut short, out of
    /// `refined_segments`.
    pub approx: Tally,
    /// Differing segments refined so far. It grows as hunks are asked for.
    pub refined_segments: u64,
    /// The first differing ranges in file order, at most 20.
    pub first_differences: Vec<Range>,
    /// Differing ranges beyond those listed.
    pub more_differences: u64,
    /// Memory budget of this diff, in bytes.
    pub mem_budget_bytes: u64,
    /// Bytes of that budget the diff holds now.
    pub mem_reserved_bytes: u64,
}

/// Answer of `hunks_at`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct HunkAnswer {
    /// Whether a difference lies within `context` lines of the line.
    pub found: bool,
    /// The range, with its context, when found.
    pub range: Option<Range>,
    /// Unified diff text of the range, when found.
    pub text: String,
    /// Diff lines in `text`.
    pub lines_shown: u64,
    /// Diff lines the whole hunk has.
    pub lines_total: u64,
    /// `text` stops before the end of the hunk.
    pub truncated: bool,
}

/// Answer of `next_difference`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Next {
    /// Whether another difference follows.
    pub found: bool,
    /// The difference, when found.
    pub range: Option<Range>,
}

/// The server: tool router and the live diffs.
#[derive(Debug, Clone)]
pub struct Server {
    sessions: Arc<Sessions>,
    tool_router: ToolRouter<Self>,
}

impl Default for Server {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl Server {
    /// A server that keeps diffs under `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            sessions: Arc::new(Sessions::new(limits)),
            tool_router: Self::tool_router(),
        }
    }

    /// The live diffs, for the idle reaper and tests.
    #[must_use]
    pub fn sessions(&self) -> &Sessions {
        &self.sessions
    }

    fn session(&self, p: &Pair) -> Result<Arc<Session>, String> {
        let mem = p
            .mem
            .as_deref()
            .map(Bytes::parse_budget)
            .transpose()
            .map_err(|e| format!("mem: {e}"))?;
        let a = FileSource::open(&p.a).map_err(|e| format!("a: {e}"))?;
        let b = FileSource::open(&p.b).map_err(|e| format!("b: {e}"))?;
        self.sessions.get(a, b, mem).map_err(|e| e.to_string())
    }
}

/// Runs `f` off the async threads: scanning and refining block.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<Json<T>, String> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| format!("internal error: {e}"))?
        .map(Json)
}

fn range(s: &Segment) -> Range {
    Range {
        a: Lines {
            start: s.a.start.0 + 1,
            count: s.len_a(),
        },
        b: Lines {
            start: s.b.start.0 + 1,
            count: s.len_b(),
        },
        status: seg_status(s.status == SegmentStatus::Approx),
    }
}

fn seg_status(approx: bool) -> String {
    if approx { "approx" } else { "differs" }.into()
}

/// Entry points the fuzz targets drive without a client: session lookup,
/// the tools and the session's diff. Only in builds with `--cfg fuzzing`,
/// so the public API is unchanged.
#[cfg(fuzzing)]
#[doc(hidden)]
impl Server {
    pub fn fuzz_session(&self, p: &Pair) -> Result<Arc<Session>, String> {
        self.session(p)
    }

    pub async fn fuzz_summary(&self, p: Pair) -> Result<Summary, String> {
        self.summary(Parameters(p)).await.map(|j| j.0)
    }

    pub async fn fuzz_hunks_at(&self, a: HunksAt) -> Result<HunkAnswer, String> {
        self.hunks_at(Parameters(a)).await.map(|j| j.0)
    }

    pub async fn fuzz_next_difference(&self, a: NextDifference) -> Result<Next, String> {
        self.next_difference(Parameters(a)).await.map(|j| j.0)
    }
}

#[tool_router]
impl Server {
    /// Sizes of the two files, how many segments are equal, differ or are
    /// approximate, and the first differing ranges.
    #[tool(
        description = "Summarize the diff of two text files: line counts, segment counts by status, the first differing ranges and memory use. Builds the diff and keeps it for the other tools."
    )]
    async fn summary(&self, Parameters(pair): Parameters<Pair>) -> Result<Json<Summary>, String> {
        let this = self.clone();
        blocking(move || {
            let s = this.session(&pair)?;
            let sum = s.summary().map_err(|e| e.to_string())?;
            let tally = |t: rift_session::Tally| Tally {
                segments: t.segments,
                a_lines: t.a_lines,
                b_lines: t.b_lines,
            };
            let all = usize::try_from(sum.differing).unwrap_or(usize::MAX);
            Ok(Summary {
                a_lines: sum.a_lines,
                b_lines: sum.b_lines,
                a_bytes: sum.a_bytes.0,
                b_bytes: sum.b_bytes.0,
                identical: sum.identical,
                equal: tally(sum.equal),
                differs: tally(sum.differs),
                approx: tally(sum.approx),
                refined_segments: sum.progress.done,
                first_differences: s
                    .differing()
                    .map_err(|e| e.to_string())?
                    .take(SUMMARY_RANGES)
                    .map(|(_, x)| range(x))
                    .collect(),
                more_differences: all.saturating_sub(SUMMARY_RANGES) as u64,
                mem_budget_bytes: sum.mem.0,
                mem_reserved_bytes: sum.reserved.0,
            })
        })
        .await
    }

    /// The difference at a line of A, as unified diff text.
    #[tool(
        description = "The difference in or next to a 1-based line of A, as unified diff text cut at `max_lines`."
    )]
    async fn hunks_at(
        &self,
        Parameters(args): Parameters<HunksAt>,
    ) -> Result<Json<HunkAnswer>, String> {
        let this = self.clone();
        blocking(move || {
            let ctx = args
                .context
                .unwrap_or(render::DEFAULT_CONTEXT)
                .min(render::MAX_CONTEXT);
            let max = args
                .max_lines
                .unwrap_or(render::DEFAULT_LINES)
                .clamp(1, render::MAX_LINES);
            let s = this.session(&args.pair)?;
            let line = args.line;
            // The first segment whose lines, widened by the context, hold the
            // line. A pure insertion holds the line after which it sits.
            let hit = s.differing().map_err(|e| e.to_string())?.find(|(_, x)| {
                x.a.start.0.saturating_sub(ctx) < line.max(1)
                    && line <= x.a.end.0.max(x.a.start.0) + ctx
            });
            let Some((i, _)) = hit else {
                return Ok(HunkAnswer {
                    found: false,
                    range: None,
                    text: String::new(),
                    lines_shown: 0,
                    lines_total: 0,
                    truncated: false,
                });
            };
            let r = render::hunk_text(s.diff().map_err(|e| e.to_string())?, i, ctx, max)?;
            let lines = |(start, count)| Lines {
                start: start + 1,
                count,
            };
            Ok(HunkAnswer {
                found: true,
                range: Some(Range {
                    a: lines(r.a),
                    b: lines(r.b),
                    status: seg_status(r.approx),
                }),
                truncated: r.shown < r.total,
                text: r.text,
                lines_shown: r.shown,
                lines_total: r.total,
            })
        })
        .await
    }

    /// The first difference that starts after a line of A.
    #[tool(
        description = "The first differing range of A that starts after the 1-based line `after_line` (0 for the first one). Pass a returned a.start to step to the next."
    )]
    async fn next_difference(
        &self,
        Parameters(args): Parameters<NextDifference>,
    ) -> Result<Json<Next>, String> {
        let this = self.clone();
        blocking(move || {
            let s = this.session(&args.pair)?;
            let next = s
                .differing()
                .map_err(|e| e.to_string())?
                .find(|(_, x)| x.a.start.0 + 1 > args.after_line);
            Ok(Next {
                found: next.is_some(),
                range: next.map(|(_, x)| range(x)),
            })
        })
        .await
    }
}

#[allow(
    clippy::unused_async_trait_impl,
    reason = "the tool_handler macro generates async fns without awaits"
)]
#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
        )
    }
}
