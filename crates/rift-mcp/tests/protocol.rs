//! Round trips through an in-process client, on small files.

use std::path::Path;
use std::time::Duration;

use rift_core::Bytes;
use rift_mcp::Server;
use rift_mcp::render::{MAX_LINES, MAX_TEXT_BYTES};
use rift_mcp::sessions::Limits;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::{RoleClient, RunningService};
use serde_json::{Value, json};

type Client = RunningService<RoleClient, ()>;

async fn connect(server: &Server) -> Client {
    let (s, c) = tokio::io::duplex(1 << 20);
    let server = server.clone();
    tokio::spawn(async move {
        server.serve(s).await.unwrap().waiting().await.unwrap();
    });
    ().serve(c).await.unwrap()
}

async fn call(c: &Client, tool: &'static str, args: Value) -> CallToolResult {
    let params = CallToolRequestParams::new(tool).with_arguments(args.as_object().unwrap().clone());
    c.call_tool(params).await.unwrap()
}

async fn ok(c: &Client, tool: &'static str, args: Value) -> Value {
    let r = call(c, tool, args).await;
    assert_ne!(r.is_error, Some(true), "{r:?}");
    r.structured_content.expect("structured content")
}

fn write(dir: &Path, name: &str, lines: impl Iterator<Item = String>) -> String {
    let path = dir.join(name);
    let text: String = lines.map(|l| l + "\n").collect();
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

/// A has 1000 lines. B changes line 500, drops lines 700 to 702 and adds a
/// line at the top.
fn pair(dir: &Path) -> (String, String) {
    let a = write(dir, "a", (1..=1000).map(|i| format!("line {i}")));
    let b = write(
        dir,
        "b",
        std::iter::once("new first".to_owned()).chain((1..=1000).filter_map(|i| match i {
            500 => Some("changed 500".to_owned()),
            700..=702 => None,
            _ => Some(format!("line {i}")),
        })),
    );
    (a, b)
}

#[tokio::test]
async fn the_server_lists_its_three_tools() {
    let c = connect(&Server::default()).await;
    assert_eq!(
        c.peer_info().unwrap().server_info.as_ref().unwrap().name,
        "rift-mcp"
    );
    let mut names: Vec<_> = c
        .list_all_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|t| t.name.to_string())
        .collect();
    names.sort();
    assert_eq!(names, ["hunks_at", "next_difference", "summary"]);
}

#[tokio::test]
async fn summary_counts_lines_and_lists_differences() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let c = connect(&Server::default()).await;
    let s = ok(&c, "summary", json!({"a": a, "b": b})).await;
    assert_eq!(
        (s["a_lines"].as_u64(), s["b_lines"].as_u64()),
        (Some(1000), Some(998))
    );
    assert_eq!(s["identical"], false);
    let first = s["first_differences"].as_array().unwrap();
    assert_eq!(first.len(), 3, "{s}");
    // A segment may take in equal lines next to a change, so check that
    // each range holds its change.
    let holds = |r: &Value, line: u64| {
        let (start, count) = (
            r["a"]["start"].as_u64().unwrap(),
            r["a"]["count"].as_u64().unwrap(),
        );
        start <= line && line < start + count.max(1)
    };
    assert!(holds(&first[0], 1));
    assert!(holds(&first[1], 500));
    assert!(holds(&first[2], 701));
    assert_eq!(s["more_differences"], 0);
    assert!(s["mem_reserved_bytes"].as_u64().unwrap() <= s["mem_budget_bytes"].as_u64().unwrap());
}

#[tokio::test]
async fn identical_files_have_no_differences() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a", (1..=50).map(|i| format!("l{i}")));
    let c = connect(&Server::default()).await;
    let s = ok(&c, "summary", json!({"a": a, "b": a})).await;
    assert_eq!(s["identical"], true);
    let n = ok(
        &c,
        "next_difference",
        json!({"a": a, "b": a, "after_line": 0}),
    )
    .await;
    assert_eq!(n["found"], false);
}

#[tokio::test]
async fn hunks_at_returns_the_unified_text_of_a_change() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let c = connect(&Server::default()).await;
    let h = ok(
        &c,
        "hunks_at",
        json!({"a": a, "b": b, "line": 500, "context": 1}),
    )
    .await;
    assert_eq!(h["found"], true);
    assert_eq!(
        h["text"],
        "@@ -499,3 +500,3 @@\n line 499\n-line 500\n+changed 500\n line 501\n"
    );
    assert_eq!(h["truncated"], false);
    // A line two lines away is outside a context of one.
    let far = ok(
        &c,
        "hunks_at",
        json!({"a": a, "b": b, "line": 600, "context": 1}),
    )
    .await;
    assert_eq!(far["found"], false);
}

#[tokio::test]
async fn hunks_at_shows_an_insertion_and_a_deletion() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let c = connect(&Server::default()).await;
    let top = ok(
        &c,
        "hunks_at",
        json!({"a": a, "b": b, "line": 1, "context": 1}),
    )
    .await;
    assert_eq!(top["text"], "@@ -1,1 +1,2 @@\n+new first\n line 1\n");
    let gone = ok(
        &c,
        "hunks_at",
        json!({"a": a, "b": b, "line": 701, "context": 0}),
    )
    .await;
    assert_eq!(
        gone["text"],
        "@@ -700,3 +700,0 @@\n-line 700\n-line 701\n-line 702\n"
    );
}

/// Different rows sharing only a repeated blank line: the hunk is
/// approximate and its header says why.
#[tokio::test]
async fn an_approximate_hunk_names_its_reason() {
    let dir = tempfile::tempdir().unwrap();
    let rows = |tag: &'static str| {
        (0..20_000).map(move |i| {
            if i % 100 == 0 {
                String::new()
            } else {
                format!("{tag} row {i}")
            }
        })
    };
    let a = write(dir.path(), "a", rows("left"));
    let b = write(dir.path(), "b", rows("right"));
    let c = connect(&Server::default()).await;
    let h = ok(
        &c,
        "hunks_at",
        json!({"a": a, "b": b, "line": 1, "context": 0, "max_lines": 1}),
    )
    .await;
    let text = h["text"].as_str().unwrap();
    let header = text.lines().next().unwrap();
    assert!(
        header.ends_with(" @@ approx: too many changes among repeated lines to align them exactly"),
        "{header}"
    );
    // The summary counts it among the refined segments.
    let sum = ok(&c, "summary", json!({"a": a, "b": b})).await;
    assert_eq!(sum["refined_segments"], 1, "{sum}");
    assert_eq!(sum["approx"]["segments"], 1, "{sum}");
    assert_eq!(sum["differs"]["segments"], 0, "{sum}");
}

#[tokio::test]
async fn next_difference_steps_through_the_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let c = connect(&Server::default()).await;
    let mut starts = vec![];
    let mut after = 0;
    loop {
        let n = ok(
            &c,
            "next_difference",
            json!({"a": a, "b": b, "after_line": after}),
        )
        .await;
        if n["found"] == false {
            break;
        }
        after = n["range"]["a"]["start"].as_u64().unwrap();
        starts.push(after);
    }
    assert_eq!(starts.len(), 3, "{starts:?}");
    assert_eq!(starts[0], 1);
    assert!(starts[1] <= 500 && 500 < starts[2], "{starts:?}");
    assert!(starts[2] <= 701, "{starts:?}");
}

#[tokio::test]
async fn a_pair_keeps_one_session() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let server = Server::default();
    let c = connect(&server).await;
    ok(&c, "summary", json!({"a": a, "b": b})).await;
    ok(
        &c,
        "next_difference",
        json!({"a": a, "b": b, "after_line": 0}),
    )
    .await;
    ok(&c, "hunks_at", json!({"a": a, "b": b, "line": 1})).await;
    assert_eq!(server.sessions().len(), 1);
}

#[tokio::test]
async fn a_changed_file_gets_a_fresh_session() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let server = Server::default();
    let c = connect(&server).await;
    let before = ok(&c, "summary", json!({"a": a, "b": b})).await;
    std::fs::write(&b, std::fs::read(&a).unwrap()).unwrap();
    let after = ok(&c, "summary", json!({"a": a, "b": b})).await;
    assert_eq!(before["identical"], false);
    assert_eq!(after["identical"], true);
}

#[tokio::test]
async fn a_missing_file_is_a_tool_error() {
    let dir = tempfile::tempdir().unwrap();
    let (a, _) = pair(dir.path());
    let c = connect(&Server::default()).await;
    let r = call(&c, "summary", json!({"a": a, "b": "/nonexistent/rift-mcp"})).await;
    assert_eq!(r.is_error, Some(true));
    assert!(format!("{r:?}").contains("No such file"), "{r:?}");
}

#[tokio::test]
async fn a_directory_is_a_tool_error() {
    let dir = tempfile::tempdir().unwrap();
    let (a, _) = pair(dir.path());
    let c = connect(&Server::default()).await;
    let d = dir.path().to_str().unwrap();
    let r = call(&c, "summary", json!({"a": a, "b": d})).await;
    assert_eq!(r.is_error, Some(true));
    assert!(format!("{r:?}").contains("not a regular file"), "{r:?}");
}

#[tokio::test]
async fn a_symlink_is_a_tool_error() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&b, &link).unwrap();
    let c = connect(&Server::default()).await;
    let r = call(&c, "summary", json!({"a": a, "b": link.to_str().unwrap()})).await;
    assert_eq!(r.is_error, Some(true));
}

/// Every line differs, and each is 1000 bytes long.
fn wide(dir: &Path) -> (String, String) {
    let line = |tag: &str, i: u32| format!("{tag} {i} {}", "x".repeat(1000));
    (
        write(dir, "wa", (0..3000).map(|i| line("old", i))),
        write(dir, "wb", (0..3000).map(|i| line("new", i))),
    )
}

#[tokio::test]
async fn max_lines_cuts_the_text_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = wide(dir.path());
    let c = connect(&Server::default()).await;
    let h = ok(
        &c,
        "hunks_at",
        json!({"a": a, "b": b, "line": 10, "max_lines": 7}),
    )
    .await;
    assert_eq!(h["lines_shown"], 7);
    assert_eq!(h["truncated"], true);
    assert!(h["lines_total"].as_u64().unwrap() > 7);
    assert_eq!(h["text"].as_str().unwrap().lines().count(), 8);
}

#[tokio::test]
async fn a_response_stays_inside_its_bounds() {
    let dir = tempfile::tempdir().unwrap();
    let (old, new) = wide(dir.path());
    let client = connect(&Server::default()).await;
    let r = call(
        &client,
        "hunks_at",
        json!({"a": old, "b": new, "line": 10, "max_lines": 1_000_000, "context": 1_000_000}),
    )
    .await;
    let answer = r.structured_content.clone().unwrap();
    let text = answer["text"].as_str().unwrap();
    assert!(answer["lines_shown"].as_u64().unwrap() <= MAX_LINES);
    // The byte bound is checked before each line, so one line may overshoot.
    assert!(text.len() < MAX_TEXT_BYTES + 512, "{}", text.len());
    // Long lines are cut and say how much is missing.
    assert!(
        text.lines().nth(1).unwrap().contains("[+"),
        "{}",
        &text[..300]
    );
    // The whole answer, structured copy and text copy together.
    assert!(serde_json::to_string(&r).unwrap().len() < 4 * MAX_TEXT_BYTES);
}

#[tokio::test]
async fn summary_lists_a_bounded_number_of_ranges() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a", (0..3000).map(|i| format!("row {i}")));
    let b = write(
        dir.path(),
        "b",
        (0..3000).map(|i| {
            if i % 10 == 5 {
                format!("odd {i}")
            } else {
                format!("row {i}")
            }
        }),
    );
    let c = connect(&Server::default()).await;
    let s = ok(&c, "summary", json!({"a": a, "b": b})).await;
    assert_eq!(
        s["first_differences"].as_array().unwrap().len(),
        rift_mcp::SUMMARY_RANGES
    );
    assert!(s["more_differences"].as_u64().unwrap() > 0);
}

fn limits(sessions: usize, total: u64) -> Limits {
    Limits {
        sessions,
        session_mem: Bytes::mib(64),
        total_mem: Bytes::mib(total),
        idle: Duration::from_secs(3600),
    }
}

#[tokio::test]
async fn memory_over_the_per_diff_limit_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let c = connect(&Server::new(limits(4, 512))).await;
    let r = call(&c, "summary", json!({"a": a, "b": b, "mem": "65M"})).await;
    assert_eq!(r.is_error, Some(true));
    let r = call(&c, "summary", json!({"a": a, "b": b, "mem": "lots"})).await;
    assert_eq!(r.is_error, Some(true));
}

#[tokio::test]
async fn the_least_recently_used_session_makes_room() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let (wa, wb) = wide(dir.path());
    let server = Server::new(limits(1, 512));
    let c = connect(&server).await;
    ok(&c, "summary", json!({"a": a, "b": b})).await;
    ok(&c, "summary", json!({"a": wa, "b": wb})).await;
    assert_eq!(server.sessions().len(), 1);
    // Same for memory: two diffs of 64M do not fit under 100M.
    let server = Server::new(limits(4, 100));
    let c = connect(&server).await;
    ok(&c, "summary", json!({"a": a, "b": b})).await;
    ok(&c, "summary", json!({"a": wa, "b": wb})).await;
    assert_eq!(server.sessions().len(), 1);
    assert!(server.sessions().mem() <= Bytes::mib(100));
}

#[tokio::test]
async fn idle_sessions_are_dropped() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let server = Server::new(Limits {
        idle: Duration::ZERO,
        ..limits(4, 512)
    });
    let c = connect(&server).await;
    ok(&c, "summary", json!({"a": a, "b": b})).await;
    assert_eq!(server.sessions().evict_idle(), 1);
    assert!(server.sessions().is_empty());
    assert_eq!(server.sessions().mem(), Bytes(0));
}
