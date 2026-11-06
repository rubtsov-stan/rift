//! rift-mcp: JSON-RPC message parsing and the three tools, through an
//! in-process client over a pipe.

use std::path::PathBuf;
use std::sync::LazyLock;

use rift_mcp::render::{MAX_LINE_BYTES, MAX_LINES, MAX_TEXT_BYTES};
use rift_mcp::{SUMMARY_RANGES, Server};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, ClientJsonRpcMessage};
use serde_json::Value;
use tokio::runtime::Runtime;

use crate::{config, fields};

const TOOLS: [&str; 3] = ["summary", "hunks_at", "next_difference"];

pub(crate) static RUNTIME: LazyLock<Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
});

/// Where A and B are written for each input: under `TMPDIR`, which the
/// Linux run points at its tmpfs scratch directory.
static DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    let dir = std::env::temp_dir().join(format!("rift-fuzz-mcp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch directory");
    dir
});

/// Writes A and B to the scratch files and returns their paths.
pub(crate) fn write_pair(a: &[u8], b: &[u8]) -> (String, String) {
    let (pa, pb) = (DIR.join("a"), DIR.join("b"));
    std::fs::write(&pa, a).expect("write a");
    std::fs::write(&pb, b).expect("write b");
    let path = |p: PathBuf| p.to_string_lossy().into_owned();
    (path(pa), path(pb))
}

/// Parses the first field as a client JSON-RPC message, then calls the
/// tool the first byte picks with the first field as its arguments, on A
/// and B written to two files.
///
/// The arguments' `a` and `b` are replaced by those two paths: a fuzzed
/// path would read whatever the host holds, and opening a FIFO blocks.
///
/// Properties: no panic or hang on any message or arguments; an answer
/// that is not an error keeps the bounds rift-mcp documents (diff text
/// within `MAX_TEXT_BYTES` plus one line, at most `MAX_LINES` lines and
/// no more than the hunk has, at most `SUMMARY_RANGES` ranges).
///
/// # Panics
///
/// On any property violation.
pub fn check(data: &[u8]) {
    let (cfg, rest) = config(data);
    let [args, a, b] = fields(rest);
    let _ = serde_json::from_slice::<ClientJsonRpcMessage>(args);
    let Ok(Value::Object(mut args)) = serde_json::from_slice::<Value>(args) else {
        return;
    };
    let (pa, pb) = write_pair(a, b);
    for (k, p) in [("a", pa), ("b", pb)] {
        args.insert(k.into(), p.into());
    }
    let tool = TOOLS[usize::from(cfg) % TOOLS.len()];

    let answer = RUNTIME.block_on(async {
        // A fresh server per input: its sessions key files by identity,
        // and A and B are rewritten in place.
        let server = Server::default();
        let (s, c) = tokio::io::duplex(1 << 20);
        let serving = tokio::spawn(async move {
            if let Ok(running) = server.serve(s).await {
                let _ = running.waiting().await;
            }
        });
        let client = ().serve(c).await.expect("handshake");
        let res = client
            .call_tool(CallToolRequestParams::new(tool).with_arguments(args))
            .await;
        let _ = client.cancel().await;
        let _ = serving.await;
        res
    });
    // A protocol-level refusal (bad arguments) is a correct answer.
    let Ok(result) = answer else { return };
    if result.is_error == Some(true) {
        return;
    }
    let v = result.structured_content.expect("structured content");
    match tool {
        "summary" => {
            let n = v["first_differences"].as_array().map_or(0, Vec::len);
            assert!(n <= SUMMARY_RANGES, "{n} ranges listed");
        }
        "hunks_at" => {
            let text = v["text"].as_str().unwrap_or_default();
            let one_line = 3 * MAX_LINE_BYTES as usize + 128;
            assert!(
                text.len() <= MAX_TEXT_BYTES + one_line,
                "{} bytes of text",
                text.len()
            );
            let (shown, total) = (v["lines_shown"].as_u64(), v["lines_total"].as_u64());
            let (shown, total) = (shown.expect("lines_shown"), total.expect("lines_total"));
            assert!(
                shown <= MAX_LINES && shown <= total,
                "{shown} of {total} lines"
            );
        }
        _ => {}
    }
}
