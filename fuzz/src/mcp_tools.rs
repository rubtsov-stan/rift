//! rift-mcp without the protocol: session lookup, the three tools and
//! `render::hunk_text`, through the `fuzz_*` entry points of `Server` and
//! `Session`.

use rift_core::{DiffView, SegmentStatus};
use rift_mcp::render::{self, MAX_CONTEXT, MAX_LINE_BYTES, MAX_LINES, MAX_TEXT_BYTES};
use rift_mcp::{HunksAt, NextDifference, Pair, SUMMARY_RANGES, Server};
use serde_json::Value;

use crate::{config, fields, mcp};

/// Differing segments rendered per input.
const RENDERED: usize = 8;

/// Text bound of one answer: `MAX_TEXT_BYTES`, plus the line that crosses
/// it (each byte at most a 3-byte U+FFFD) and its length note.
fn text_bound() -> usize {
    MAX_TEXT_BYTES + 3 * MAX_LINE_BYTES as usize + 128
}

/// Calls the tool the first byte picks with the first field as its
/// arguments, on A and B written to two scratch files (the arguments' `a`
/// and `b` are replaced, as in `mcp_request`), then renders up to eight
/// differing segments of the session with `hunk_text`, with the context
/// and line limit the arguments give, clamped as `hunks_at` clamps them.
///
/// Properties: no panic or hang; answers keep the documented bounds; a
/// rendered hunk shows at most the lines asked for and the hunk has, one
/// text line per shown line after its header, within the text bound.
///
/// # Panics
///
/// On any property violation.
pub fn check(data: &[u8]) {
    let (cfg, rest) = config(data);
    let [args, a, b] = fields(rest);
    let Ok(Value::Object(mut args)) = serde_json::from_slice::<Value>(args) else {
        return;
    };
    let (pa, pb) = mcp::write_pair(a, b);
    for (k, p) in [("a", pa), ("b", pb)] {
        args.insert(k.into(), p.into());
    }
    let args = Value::Object(args);
    let server = Server::default();

    mcp::RUNTIME.block_on(async {
        match cfg % 3 {
            0 => {
                if let Ok(p) = serde_json::from_value::<Pair>(args.clone())
                    && let Ok(s) = server.fuzz_summary(p).await
                {
                    assert!(s.first_differences.len() <= SUMMARY_RANGES);
                    assert_eq!(s.identical, s.first_differences.is_empty());
                }
            }
            1 => {
                if let Ok(h) = serde_json::from_value::<HunksAt>(args.clone())
                    && let Ok(r) = server.fuzz_hunks_at(h).await
                {
                    assert!(r.text.len() <= text_bound(), "{} bytes", r.text.len());
                    assert!(r.lines_shown <= MAX_LINES && r.lines_shown <= r.lines_total);
                    assert_eq!(r.truncated, r.lines_shown < r.lines_total);
                    assert_eq!(r.found, r.range.is_some());
                }
            }
            _ => {
                if let Ok(n) = serde_json::from_value::<NextDifference>(args.clone())
                    && let Ok(r) = server.fuzz_next_difference(n).await
                {
                    assert_eq!(r.found, r.range.is_some());
                }
            }
        }
    });

    let Ok(pair) = serde_json::from_value::<Pair>(args.clone()) else {
        return;
    };
    let Ok(session) = server.fuzz_session(&pair) else {
        return;
    };
    let diff = session.fuzz_diff();
    let num = |k: &str| args.get(k).and_then(Value::as_u64);
    let ctx = num("context")
        .unwrap_or(render::DEFAULT_CONTEXT)
        .min(MAX_CONTEXT);
    let max = num("max_lines")
        .unwrap_or(render::DEFAULT_LINES)
        .clamp(1, MAX_LINES);
    let differing = diff
        .segments()
        .iter()
        .enumerate()
        .filter(|(_, s)| s.status != SegmentStatus::Equal)
        .map(|(i, _)| i)
        .take(RENDERED)
        .collect::<Vec<_>>();
    for i in differing {
        let Ok(r) = render::hunk_text(diff, i, ctx, max) else {
            continue;
        };
        assert!(
            r.shown <= max && r.shown <= r.total,
            "{} of {} lines",
            r.shown,
            r.total
        );
        assert!(r.text.len() <= text_bound(), "{} bytes", r.text.len());
        let lines = r.text.matches('\n').count() as u64;
        assert_eq!(lines, r.shown + 1, "text lines of segment {i}");
    }
}
