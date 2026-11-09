//! The methods of the protocol: their params, checked before a request
//! thread starts, and their answers.
//!
//! Large answers are not built: a page of segments or moved blocks is
//! serialized by the writer straight from the session's map, and a hunk
//! from its [`Excerpt`], whose lines live in the session's response pool
//! until the writer has written them.

use std::ops::Range;
use std::sync::Arc;

use rift_core::{DiffView, HunkState, LineNo, Segment, SegmentStatus};
use rift_session::{
    At, Column, DEFAULT_CONTEXT, DEFAULT_LINE_BYTES, DEFAULT_LINES, DEFAULT_PAGE, Eol, Excerpt,
    ExcerptRequest, Line, MAX_CONTEXT, MAX_LINE_BYTES, MAX_LINES, MAX_PAGE, MovesState, Session,
    SessionId, SessionState,
};
use serde::Serialize;
use serde::ser::{SerializeStruct, Serializer};
use serde_json::{Value, json};

use super::wire::{
    Body, Params, Reply, RpcError, invalid, parse_mem, scan_progress, session_error,
};
use super::{Flight, Server};

/// A request whose params have been checked.
#[derive(Debug)]
pub enum Call {
    Open {
        a: String,
        b: String,
        mem: Option<String>,
    },
    Summary(SessionId),
    Segments(SessionId, Page),
    Hunk(SessionId, ExcerptRequest),
    Moves(SessionId, Page),
    SetVisible(SessionId, Range<u64>),
    Close(SessionId),
}

#[derive(Debug, Clone, Copy)]
pub struct Page {
    offset: usize,
    limit: usize,
}

/// The methods `initialize` lists, in its order.
pub const METHODS: [&str; 8] = [
    "initialize",
    "open",
    "summary",
    "segments",
    "hunk",
    "moves",
    "set_visible",
    "close",
];

fn session_param(p: &Params) -> Result<SessionId, RpcError> {
    p.need_count("session", 1).map(SessionId)
}

fn page(p: &Params) -> Result<Page, RpcError> {
    let offset = p.count("offset", 0)?.unwrap_or(0);
    let limit = p.count("limit", 1)?.unwrap_or(DEFAULT_PAGE as u64);
    Ok(Page {
        offset: usize::try_from(offset).unwrap_or(usize::MAX),
        limit: usize::try_from(limit).unwrap_or(usize::MAX).min(MAX_PAGE),
    })
}

/// Checks the params of `method`, any method but `initialize`.
///
/// # Errors
///
/// `invalid_params` naming what does not match the method's schema.
pub fn parse(method: &str, params: Option<Value>) -> Result<Call, RpcError> {
    const PAGE: &[&str] = &["session", "offset", "limit"];
    let names: &[&str] = match method {
        "open" => &["a", "b", "mem"],
        "segments" | "moves" => PAGE,
        "hunk" => &[
            "session",
            "segment",
            "line_a",
            "context",
            "max_lines",
            "max_line_bytes",
        ],
        "set_visible" => &["session", "a"],
        _ => &["session"],
    };
    let p = Params::new(params, names)?;
    Ok(match method {
        "open" => {
            let path = |name| match p.text(name)? {
                Some(s) if !s.is_empty() => Ok(s.to_owned()),
                _ => Err(invalid(name)),
            };
            Call::Open {
                a: path("a")?,
                b: path("b")?,
                mem: p.text("mem")?.map(str::to_owned),
            }
        }
        "summary" => Call::Summary(session_param(&p)?),
        "close" => Call::Close(session_param(&p)?),
        "segments" => Call::Segments(session_param(&p)?, page(&p)?),
        "moves" => Call::Moves(session_param(&p)?, page(&p)?),
        "hunk" => {
            let at = match (p.count("segment", 0)?, p.count("line_a", 1)?) {
                (Some(i), None) => At::Segment(usize::try_from(i).unwrap_or(usize::MAX)),
                (None, Some(line)) => At::LineA(LineNo(line - 1)),
                _ => return Err(invalid("segment")),
            };
            Call::Hunk(
                session_param(&p)?,
                ExcerptRequest {
                    at,
                    context: p
                        .count("context", 0)?
                        .unwrap_or(DEFAULT_CONTEXT)
                        .min(MAX_CONTEXT),
                    max_lines: p
                        .count("max_lines", 1)?
                        .unwrap_or(DEFAULT_LINES)
                        .min(MAX_LINES),
                    max_line_bytes: p
                        .count("max_line_bytes", 1)?
                        .unwrap_or(DEFAULT_LINE_BYTES)
                        .min(MAX_LINE_BYTES),
                },
            )
        }
        "set_visible" => {
            let a = p
                .object("a", &["start", "count"])?
                .ok_or_else(|| invalid("a"))?;
            let start = a.need_count("start", 1).map_err(|_| invalid("a"))? - 1;
            let count = a.need_count("count", 0).map_err(|_| invalid("a"))?;
            Call::SetVisible(session_param(&p)?, start..start + count)
        }
        _ => unreachable!("only the methods listed reach parse"),
    })
}

impl Server {
    /// Answers `call` for `flight`.
    pub fn run(&self, flight: &Flight, call: Call) {
        let answer: Result<Box<dyn Body>, RpcError> = match call {
            Call::Close(id) => {
                // Under the notifier's lock, so no notification about the
                // session follows the answer.
                let notes = self
                    .notes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let closed = self.sessions.close(id);
                self.reply(
                    flight,
                    Box::new(super::wire::result(&flight.id, json!({"closed": closed}))),
                );
                drop(notes);
                return;
            }
            Call::Open { a, b, mem } => self.open(&flight.id, &a, &b, mem.as_deref()),
            Call::Summary(id) => self.summary(&flight.id, id),
            Call::Segments(id, page) => self.segments(&flight.id, id, page),
            Call::Moves(id, page) => self.moves(&flight.id, id, page),
            Call::Hunk(id, req) => self.hunk(&flight.id, id, &req),
            Call::SetVisible(id, lines) => self.set_visible(&flight.id, id, lines),
        };
        let body = answer.unwrap_or_else(|e| Box::new(e.answer(&flight.id)));
        self.reply(flight, body);
    }

    fn session(&self, id: SessionId) -> Result<Arc<Session>, RpcError> {
        self.sessions
            .session(id)
            .map_err(|e| session_error(&e, None))
    }

    /// The session `id`, which must be ready.
    fn ready(&self, id: SessionId) -> Result<Arc<Session>, RpcError> {
        let s = self.session(id)?;
        if s.state() == SessionState::Ready {
            Ok(s)
        } else {
            let e = rift_session::Error::NotReady {
                session: id,
                state: s.state(),
            };
            Err(session_error(&e, Some(&s)))
        }
    }

    fn open(
        &self,
        id: &Value,
        a: &str,
        b: &str,
        mem: Option<&str>,
    ) -> Result<Box<dyn Body>, RpcError> {
        let mem = match mem {
            Some(m) => parse_mem(m)?,
            None => self.default_mem,
        };
        let opened = self
            .sessions
            .open(a, b, Some(mem))
            .map_err(|e| session_error(&e, None))?;
        let s = &opened.session;
        // A failed session is replaced, so a reused one is scanning or
        // ready; one that failed since is reported as it was found.
        let state = if opened.reused && s.state() == SessionState::Ready {
            "ready"
        } else {
            "scanning"
        };
        Ok(Box::new(super::wire::result(
            id,
            json!({"session": s.id().0, "reused": opened.reused, "mem_bytes": s.mem().0, "state": state}),
        )))
    }

    fn summary(&self, id: &Value, sid: SessionId) -> Result<Box<dyn Body>, RpcError> {
        let s = self.session(sid)?;
        let result = match s.summary() {
            Ok(sum) => {
                let tally = |t: rift_session::Tally| json!({"segments": t.segments, "a_lines": t.a_lines, "b_lines": t.b_lines});
                json!({
                    "state": "ready",
                    "a": {"lines": sum.a_lines, "bytes": sum.a_bytes.0},
                    "b": {"lines": sum.b_lines, "bytes": sum.b_bytes.0},
                    "identical": sum.identical,
                    "equal": tally(sum.equal),
                    "differs": tally(sum.differs),
                    "approx": tally(sum.approx),
                    "differing_segments": sum.differing,
                    "moves": moves_name(sum.moves),
                    "moved_blocks": sum.moved_blocks,
                    "mem_bytes": sum.mem.0,
                    "mem_reserved_bytes": sum.reserved.0,
                    "progress": {"phase": "refine", "done": sum.progress.done, "total": sum.progress.total},
                })
            }
            Err(_) => {
                if let Some(err) = s.error() {
                    json!({"state": "failed", "error": session_error(err, None).object()})
                } else {
                    let [a, b] = s.bytes();
                    json!({
                        "state": "scanning",
                        "a": {"bytes": a.0},
                        "b": {"bytes": b.0},
                        "mem_bytes": s.mem().0,
                        "progress": scan_progress(&s),
                    })
                }
            }
        };
        Ok(Box::new(super::wire::result(id, result)))
    }

    fn segments(&self, id: &Value, sid: SessionId, page: Page) -> Result<Box<dyn Body>, RpcError> {
        let session = self.ready(sid)?;
        let total = session
            .differing()
            .map_err(|e| session_error(&e, None))?
            .count();
        Ok(Box::new(Reply {
            id: id.clone(),
            result: SegmentsPage {
                session,
                page,
                total,
            },
        }))
    }

    fn moves(&self, id: &Value, sid: SessionId, page: Page) -> Result<Box<dyn Body>, RpcError> {
        let session = self.ready(sid)?;
        Ok(Box::new(Reply {
            id: id.clone(),
            result: MovesPage { session, page },
        }))
    }

    fn hunk(
        &self,
        id: &Value,
        sid: SessionId,
        req: &ExcerptRequest,
    ) -> Result<Box<dyn Body>, RpcError> {
        let session = self.ready(sid)?;
        let excerpt = session
            .excerpt(req)
            .map_err(|e| session_error(&e, Some(&session)))?;
        if excerpt.pending {
            self.watch_segment(sid, excerpt.segment);
        }
        Ok(Box::new(Reply {
            id: id.clone(),
            result: HunkOut(excerpt),
        }))
    }

    fn set_visible(
        &self,
        id: &Value,
        sid: SessionId,
        lines: Range<u64>,
    ) -> Result<Box<dyn Body>, RpcError> {
        let session = self.ready(sid)?;
        let diff = session.diff().map_err(|e| session_error(&e, None))?;
        let shown = LineNo(lines.start)..LineNo(lines.end.max(lines.start + 1));
        diff.set_visible(shown.clone());
        let segs = diff.segments();
        let pending: Vec<usize> = touching(segs, shown)
            .filter(|&i| segs[i].status != SegmentStatus::Equal)
            .filter(|&i| matches!(diff.try_hunk(i), Ok(HunkState::Pending)))
            .take(MAX_PAGE)
            .collect();
        self.show_segments(sid, pending);
        Ok(Box::new(super::wire::result(id, Value::Null)))
    }
}

/// Indices of the segments whose A lines meet `lines`, counting a pure
/// insertion at the A line it follows (the first line for one at the
/// start).
pub fn touching(segs: &[Segment], lines: Range<LineNo>) -> Range<usize> {
    // Both are non-decreasing along the map, since segments tile A in
    // order.
    let first = |x: &Segment| {
        if x.a.is_empty() {
            x.a.start.0.saturating_sub(1)
        } else {
            x.a.start.0
        }
    };
    let last = |x: &Segment| {
        if x.a.is_empty() {
            first(x)
        } else {
            x.a.end.0 - 1
        }
    };
    let lo = segs.partition_point(|x| last(x) < lines.start.0);
    let hi = segs.partition_point(|x| first(x) < lines.end.0);
    lo..hi.max(lo)
}

fn moves_name(m: MovesState) -> &'static str {
    match m {
        MovesState::Found => "found",
        MovesState::Off => "off",
        MovesState::Skipped => "skipped",
    }
}

/// The status name of a refined segment: `differs` or `approx`.
pub fn refined_status(state: &HunkState, seg: &Segment) -> Option<&'static str> {
    match state {
        HunkState::Pending => None,
        HunkState::Approx(_) => Some("approx"),
        HunkState::Ready(_) => Some(status_name(seg.status)),
    }
}

fn status_name(s: SegmentStatus) -> &'static str {
    match s {
        SegmentStatus::Equal => "equal",
        SegmentStatus::Differs => "differs",
        SegmentStatus::Approx => "approx",
    }
}

#[derive(Serialize)]
struct RangeOut {
    start: u64,
    count: u64,
}

fn range(lines: &Range<LineNo>) -> RangeOut {
    RangeOut {
        start: lines.start.0 + 1,
        count: lines.end.0 - lines.start.0,
    }
}

/// A range on each side: a moved block, or a hunk's segment.
#[derive(Serialize)]
struct Pair {
    a: RangeOut,
    b: RangeOut,
}

#[derive(Serialize)]
struct SegmentOut {
    index: usize,
    a: RangeOut,
    b: RangeOut,
    status: &'static str,
}

/// A page of differing segments, read from the map as it is written.
struct SegmentsPage {
    session: Arc<Session>,
    page: Page,
    total: usize,
}

impl Serialize for SegmentsPage {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let mut st = ser.serialize_struct("segments", 3)?;
        st.serialize_field("total", &self.total)?;
        st.serialize_field("offset", &self.page.offset)?;
        st.serialize_field(
            "segments",
            &Seq(|| {
                self.session
                    .differing()
                    .into_iter()
                    .flatten()
                    .skip(self.page.offset)
                    .take(self.page.limit)
                    .map(|(index, s)| SegmentOut {
                        index,
                        a: range(&s.a),
                        b: range(&s.b),
                        // Refinement can turn a differing segment approx;
                        // the map does not know until it has run.
                        status: if self
                            .session
                            .diff()
                            .is_ok_and(|d| d.refined_approx(index) == Some(true))
                        {
                            "approx"
                        } else {
                            status_name(s.status)
                        },
                    })
            }),
        )?;
        st.end()
    }
}

/// Serializes the items an iterator yields, as a JSON array, without
/// collecting them.
struct Seq<F>(F);

impl<F, I> Serialize for Seq<F>
where
    F: Fn() -> I,
    I: Iterator,
    I::Item: Serialize,
{
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        ser.collect_seq((self.0)())
    }
}

/// A page of moved blocks, read from the block list as it is written.
struct MovesPage {
    session: Arc<Session>,
    page: Page,
}

impl Serialize for MovesPage {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let diff = self.session.diff().ok();
        let blocks = diff.map_or(&[][..], DiffView::moved_blocks);
        let state = match diff.map(rift_core::diff::Diff::moves) {
            Some(m) if m.disabled() => "off",
            Some(m) if m.skipped() => "skipped",
            _ => "found",
        };
        let mut st = ser.serialize_struct("moves", 4)?;
        st.serialize_field("total", &blocks.len())?;
        st.serialize_field("offset", &self.page.offset)?;
        st.serialize_field("moves", state)?;
        st.serialize_field(
            "blocks",
            &Seq(|| {
                blocks
                    .iter()
                    .skip(self.page.offset)
                    .take(self.page.limit)
                    .map(|m| Pair {
                        a: range(&m.a),
                        b: range(&m.b),
                    })
            }),
        )?;
        st.end()
    }
}

/// A hunk, written from the excerpt that holds its lines.
struct HunkOut(Excerpt);

impl Serialize for HunkOut {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let e = &self.0;
        let mut st = ser.serialize_struct("hunk", 7)?;
        st.serialize_field("segment", &e.segment)?;
        st.serialize_field("status", status_name(e.status))?;
        st.serialize_field("pending", &e.pending)?;
        let pair = Pair {
            a: range(&e.range.a),
            b: range(&e.range.b),
        };
        st.serialize_field("range", &pair)?;
        st.serialize_field("a", &ColumnOut(&e.a))?;
        st.serialize_field("b", &ColumnOut(&e.b))?;
        st.serialize_field("truncated", &e.truncated())?;
        st.end()
    }
}

struct ColumnOut<'a>(&'a Column);

impl Serialize for ColumnOut<'_> {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        let mut st = ser.serialize_struct("column", 3)?;
        st.serialize_field("start", &(self.0.start.0 + 1))?;
        st.serialize_field("total", &self.0.total)?;
        st.serialize_field("lines", &Seq(|| self.0.lines.iter().map(LineOut::of)))?;
        st.end()
    }
}

#[derive(Serialize)]
struct LineOut<'a> {
    text: &'a str,
    bytes: u64,
    eol: &'static str,
    #[serde(skip_serializing_if = "is_false")]
    truncated: bool,
    #[serde(skip_serializing_if = "is_false")]
    lossy: bool,
    #[serde(skip_serializing_if = "is_false")]
    moved: bool,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes a reference"
)]
fn is_false(b: &bool) -> bool {
    !*b
}

impl<'a> LineOut<'a> {
    fn of(l: &'a Line) -> Self {
        Self {
            text: &l.text,
            bytes: l.bytes,
            eol: match l.eol {
                Eol::Lf => "lf",
                Eol::CrLf => "crlf",
                Eol::None => "none",
            },
            truncated: l.truncated,
            lossy: l.lossy,
            moved: l.moved,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(a: Range<u64>, b: Range<u64>) -> Segment {
        Segment {
            a: LineNo(a.start)..LineNo(a.end),
            b: LineNo(b.start)..LineNo(b.end),
            status: SegmentStatus::Differs,
        }
    }

    #[test]
    fn an_insertion_counts_at_the_line_it_follows() {
        // Lines 0..5, an insertion after line 4, lines 5..9, an insertion
        // at the very start of A comes first.
        let segs = [
            seg(0..0, 0..2),
            seg(0..5, 2..7),
            seg(5..5, 7..9),
            seg(5..9, 9..13),
        ];
        let at = |s: u64, e: u64| touching(&segs, LineNo(s)..LineNo(e));
        assert_eq!(at(0, 1), 0..2);
        assert_eq!(at(4, 5), 1..3);
        assert_eq!(at(5, 6), 3..4);
        assert_eq!(at(1, 3), 1..2);
        assert_eq!(at(9, 20), 4..4);
    }
}
