//! `rift serve` end to end: every method over its standard input and
//! output, each transcript checked against the JSON schemas of
//! `docs/protocol/`.
#![allow(
    clippy::many_single_char_names,
    reason = "c is the client, s a session, a and b the files, as in the protocol"
)]

#[path = "serve/client.rs"]
mod client;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use client::Client;
use serde_json::{Value, json};

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const NOT_INITIALIZED: i64 = -32002;
const FILE: i64 = -32001;
const BUDGET_EXCEEDED: i64 = -32003;
const TOO_LARGE: i64 = -32004;
const UNKNOWN_SESSION: i64 = -32006;
const NOT_READY: i64 = -32008;

fn write(dir: &Path, name: &str, bytes: &[u8]) -> String {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path.to_str().unwrap().to_owned()
}

fn numbered(tag: &str, range: std::ops::Range<u32>) -> Vec<u8> {
    range
        .flat_map(|i| format!("{tag} {i}\n").into_bytes())
        .collect()
}

/// A file that takes seconds to scan and nothing to make: one line of
/// 1 GiB, sparse on disk.
fn huge(dir: &Path) -> String {
    let path = dir.join("slow");
    std::fs::File::create(&path)
        .unwrap()
        .set_len(1 << 30)
        .unwrap();
    path.to_str().unwrap().to_owned()
}

/// A generated pair of about `size` bytes per file.
fn generated(dir: &Path, size: u64) -> (PathBuf, PathBuf) {
    let (a, b) = (dir.join("gen-a.sql"), dir.join("gen-b.sql"));
    let config = rift_gen::Config::new(7, size, rift_gen::Profile::Sql);
    rift_gen::generate(
        &config,
        std::fs::File::create(&a).unwrap(),
        std::fs::File::create(&b).unwrap(),
        std::io::sink(),
    )
    .unwrap();
    (a, b)
}

fn lines_of(path: &Path) -> Vec<String> {
    std::fs::read(path)
        .unwrap()
        .split_inclusive(|&c| c == b'\n')
        .map(|l| {
            let l = l.strip_suffix(b"\n").unwrap_or(l);
            String::from_utf8_lossy(l.strip_suffix(b"\r").unwrap_or(l)).into_owned()
        })
        .collect()
}

fn session(opened: &Value) -> u64 {
    opened["session"].as_u64().unwrap()
}

/// Opens `a` and `b` and waits until the session is ready.
fn open_ready(c: &mut Client, a: &str, b: &str, mem: &str) -> u64 {
    let opened = c.ok("open", json!({"a": a, "b": b, "mem": mem}));
    let s = session(&opened);
    let state = c.state_of(s);
    assert_eq!(state["state"], "ready", "{state}");
    s
}

/// The peak heap `--stats` printed.
fn peak_heap(stderr: &str) -> u64 {
    let peak = stderr
        .split_whitespace()
        .skip_while(|w| *w != "heap")
        .nth(1)
        .and_then(|w| w.parse().ok())
        .unwrap_or_else(|| panic!("no peak heap in {stderr:?}"));
    eprintln!("peak heap {peak} bytes");
    peak
}

fn clean_exit(c: Client) -> String {
    let (status, stderr) = c.finish();
    assert_eq!(status.code(), Some(0), "{stderr}");
    stderr
}

#[test]
fn initialize_comes_first_and_reports_the_limits() {
    let mut c = Client::start(&["--max-session-mem", "512M", "--max-total-mem", "1G"]);
    c.err("summary", json!({"session": 1}), NOT_INITIALIZED);
    let e = c.err("initialize", json!({"protocol": 2}), INVALID_PARAMS);
    assert_eq!(e["data"]["supported"], json!([1]));
    let init = c.initialize();
    assert_eq!(init["protocol"], 1);
    assert_eq!(init["server"]["name"], "rift");
    assert_eq!(init["server"]["version"], env!("CARGO_PKG_VERSION"));
    let methods = [
        "initialize",
        "open",
        "summary",
        "segments",
        "hunk",
        "moves",
        "set_visible",
        "close",
    ];
    assert_eq!(init["capabilities"]["methods"], json!(methods));
    assert_eq!(
        init["capabilities"]["notifications"],
        json!(["progress", "session_state", "segment_ready"])
    );
    assert_eq!(
        init["limits"],
        json!({
            "max_sessions": 4,
            "max_session_mem": 512u64 << 20,
            "max_total_mem": 1u64 << 30,
            "default_mem": 256u64 << 20,
            "idle_timeout_secs": null,
            "max_page": 1000,
            "max_lines": 5000,
            "max_line_bytes": 65536,
            "max_context": 100
        })
    );
    c.err("nope", json!({}), METHOD_NOT_FOUND);
    clean_exit(c);
}

#[test]
fn the_defaults_are_one_and_two_gibibytes() {
    let mut c = Client::start(&[]);
    let limits = c.initialize()["limits"].clone();
    assert_eq!(limits["max_session_mem"], 1u64 << 30);
    assert_eq!(limits["max_total_mem"], 2u64 << 30);
    clean_exit(c);
}

#[test]
#[allow(clippy::too_many_lines, reason = "one session, step by step")]
fn a_scripted_session_on_a_generated_pair() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = generated(dir.path(), 4 << 20);
    let (pa, pb) = (a.to_str().unwrap(), b.to_str().unwrap());
    let mut c = Client::start(&[]);
    c.initialize();
    let opened = c.ok("open", json!({"a": pa, "b": pb, "mem": "64M"}));
    assert_eq!(opened["reused"], false);
    assert_eq!(opened["state"], "scanning");
    assert_eq!(opened["mem_bytes"], 64u64 << 20);
    let s = session(&opened);
    assert_eq!(c.state_of(s)["state"], "ready");
    let scans: Vec<Value> = c
        .notes_of("progress", s)
        .into_iter()
        .filter(|p| p["phase"] == "scan")
        .collect();
    let last = scans.last().expect("scan progress");
    assert_eq!(last["done"], last["total"]);
    let reopened = c.ok("open", json!({"a": pa, "b": pb}));
    assert_eq!(
        reopened,
        json!({"session": s, "reused": true, "mem_bytes": 64u64 << 20, "state": "ready"})
    );

    let sum = c.ok("summary", json!({"session": s}));
    assert_eq!(sum["state"], "ready");
    let (la, lb) = (lines_of(&a), lines_of(&b));
    assert_eq!(sum["a"]["lines"], la.len());
    assert_eq!(sum["b"]["lines"], lb.len());
    let differing = sum["differing_segments"].as_u64().unwrap();
    assert!(differing > 1, "{sum}");
    assert_eq!(sum["identical"], false);

    // Pages cover every differing segment once, in file order.
    let mut segments = Vec::new();
    while (segments.len() as u64) < differing {
        let page = c.ok(
            "segments",
            json!({"session": s, "offset": segments.len(), "limit": 7}),
        );
        assert_eq!(page["total"], differing);
        assert_eq!(page["offset"], segments.len());
        let got = page["segments"].as_array().unwrap().clone();
        assert!(!got.is_empty() && got.len() <= 7);
        segments.extend(got);
    }
    let page = c.ok(
        "segments",
        json!({"session": s, "offset": differing, "limit": 1}),
    );
    assert_eq!(page["segments"], json!([]));
    assert!(
        segments
            .windows(2)
            .all(|w| w[0]["index"].as_u64() < w[1]["index"].as_u64())
    );

    // Hunks hold the lines of the files at the numbers they say.
    let mut pending = Vec::new();
    for seg in segments.iter().take(12) {
        let h = c.ok(
            "hunk",
            json!({"session": s, "segment": seg["index"], "context": 2}),
        );
        if h["pending"] == true {
            pending.push(h["segment"].clone());
        }
        assert_eq!(h["segment"], seg["index"]);
        assert_eq!(h["range"]["a"], seg["a"]);
        assert_eq!(h["range"]["b"], seg["b"]);
        for (side, lines) in [("a", &la), ("b", &lb)] {
            let col = &h[side];
            let start = usize::try_from(col["start"].as_u64().unwrap()).unwrap();
            for (k, line) in col["lines"].as_array().unwrap().iter().enumerate() {
                if line.get("truncated").is_none() {
                    assert_eq!(
                        line["text"],
                        lines[start - 1 + k],
                        "{side} line {}",
                        start + k
                    );
                }
            }
        }
    }
    // Each pending hunk is reported once its refinement ends.
    for index in &pending {
        c.until("segment_ready", |c| {
            c.notes_of("segment_ready", s)
                .iter()
                .any(|n| n["index"] == *index)
        });
    }
    let ready = c.notes_of("segment_ready", s);
    for n in &ready {
        assert_eq!(
            ready.iter().filter(|m| m["index"] == n["index"]).count(),
            1,
            "{n}"
        );
    }
    let first = &segments[0];
    let at = first["a"]["start"].as_u64().unwrap().max(1);
    let by_line = c.ok("hunk", json!({"session": s, "line_a": at}));
    assert!(by_line["range"]["a"]["start"].as_u64().unwrap() <= at);

    let moves = c.ok("moves", json!({"session": s, "limit": 5}));
    assert_eq!(moves["moves"], "found");
    assert_eq!(moves["offset"], 0);
    assert!(moves["blocks"].as_array().unwrap().len() <= 5);

    // set_visible: the segments it shows are reported when refined.
    let last_seg = segments.last().unwrap();
    let visible = json!({"start": last_seg["a"]["start"], "count": 3});
    assert_eq!(
        c.ok("set_visible", json!({"session": s, "a": visible})),
        Value::Null
    );
    c.until("refinement to finish", |c| {
        c.notes_of("progress", s)
            .iter()
            .any(|p| p["phase"] == "refine" && p["done"] == p["total"])
    });
    for seg in &segments {
        let h = c.ok(
            "hunk",
            json!({"session": s, "segment": seg["index"], "max_lines": 1}),
        );
        assert_eq!(h.get("pending"), Some(&json!(false)), "{h}");
    }

    assert_eq!(
        c.ok("close", json!({"session": s})),
        json!({"closed": true})
    );
    assert_eq!(
        c.ok("close", json!({"session": s})),
        json!({"closed": false})
    );
    let unknown = c.err("summary", json!({"session": s}), UNKNOWN_SESSION);
    assert_eq!(unknown["data"], json!({"session": s}));
    let again = c.ok("open", json!({"a": pa, "b": pb}));
    assert!(session(&again) > s && again["reused"] == false, "{again}");
    clean_exit(c);
}

#[test]
fn scan_progress_grows_to_its_total_at_most_20_times_a_second() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = generated(dir.path(), 48 << 20);
    let mut c = Client::start(&[]);
    c.initialize();
    let began = Instant::now();
    let s = open_ready(&mut c, a.to_str().unwrap(), b.to_str().unwrap(), "64M");
    let took = began.elapsed();
    let scans: Vec<(Instant, u64, u64)> = c
        .notes
        .iter()
        .filter(|(_, n)| {
            n["method"] == "progress"
                && n["params"]["session"] == s
                && n["params"]["phase"] == "scan"
        })
        .map(|(t, n)| {
            let p = &n["params"];
            (
                *t,
                p["done"].as_u64().unwrap(),
                p["total"].as_u64().unwrap(),
            )
        })
        .collect();
    let size = std::fs::metadata(&a).unwrap().len() + std::fs::metadata(&b).unwrap().len();
    assert!(
        scans.len() > 2,
        "{} scan notifications in {took:?}",
        scans.len()
    );
    assert!(scans.iter().all(|&(_, _, total)| total == 2 * size));
    assert!(
        scans.windows(2).all(|w| w[0].1 <= w[1].1),
        "progress went back"
    );
    assert_eq!(scans.last().unwrap().1, 2 * size);
    // The state comes after the last scan progress.
    let state_at = c
        .notes
        .iter()
        .position(|(_, n)| n["method"] == "session_state")
        .unwrap();
    let last_scan = c
        .notes
        .iter()
        .rposition(|(_, n)| n["method"] == "progress" && n["params"]["phase"] == "scan")
        .unwrap();
    assert!(last_scan < state_at);
    let limit = 20.0 * took.as_secs_f64() + 3.0;
    assert!(
        f64::from(u32::try_from(scans.len()).unwrap()) <= limit,
        "{} notifications in {took:?}",
        scans.len()
    );
    clean_exit(c);
}

/// The scenarios `editors/vscode/test/check-mock.js` plays against the mock
/// server, on real files.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "the steps of check-mock.js, in its order"
)]
fn the_mock_scenarios_behave_the_same_on_the_real_server() {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    // A: 60 numbered lines, with a moved block of five, a 10 KB line and
    // an equal stretch; B changes some lines, moves the block, and has an
    // invalid UTF-8 line.
    let block = numbered("moved row", 0..5);
    let mut a = numbered("line", 1..21);
    a.extend(&block);
    a.extend(numbered("line", 21..41));
    a.extend(format!("{}\n", "x".repeat(10_000)).as_bytes());
    a.extend(numbered("line", 41..61));
    let mut b = numbered("line", 1..8);
    b.extend(b"changed\n");
    b.extend(numbered("line", 9..21));
    b.extend(numbered("line", 21..41));
    b.extend(b"caf\xe9\n");
    b.extend(numbered("line", 41..61));
    b.extend(numbered("new", 0..400));
    b.extend(&block);
    let (pa, pb) = (write(d, "a.sql", &a), write(d, "b.sql", &b));
    let missing = d.join("missing.sql").to_str().unwrap().to_owned();
    let small_a = write(d, "big.sql", &numbered("row", 0..20_000));
    let small_b = write(d, "small.sql", b"x\n");
    let slow = huge(d);

    let mut c = Client::start(&[]);
    c.err("summary", json!({"session": 1}), NOT_INITIALIZED);
    assert_eq!(c.initialize()["protocol"], 1);
    c.err("initialize", json!({"protocol": 2}), INVALID_PARAMS);
    c.err("nope", json!({}), METHOD_NOT_FOUND);

    // Errors open itself reports.
    let file = c.err("open", json!({"a": pa, "b": missing}), FILE);
    assert_eq!(
        file["data"],
        json!({"side": "b", "path": missing, "reason": "not_found"})
    );
    let too_large = c.err("open", json!({"a": pa, "b": pb, "mem": "100G"}), TOO_LARGE);
    assert_eq!(
        too_large["data"],
        json!({"mem_bytes": 100u64 << 30, "max_mem_bytes": 1u64 << 30})
    );
    let e = c.err(
        "open",
        json!({"a": pa, "b": pb, "mem": "lots"}),
        INVALID_PARAMS,
    );
    assert_eq!(e["data"]["param"], "mem");

    // A scan that fails: open answers, the session fails, reopening with
    // the suggested budget replaces it.
    let failing = c.ok("open", json!({"a": small_a, "b": small_b, "mem": "64K"}));
    assert_eq!(failing["state"], "scanning");
    let fs = session(&failing);
    let failed = c.state_of(fs);
    assert_eq!(failed["state"], "failed");
    let budget = failed["error"].clone();
    assert_eq!(budget["code"], BUDGET_EXCEEDED);
    let data = &budget["data"];
    assert_eq!(data["mem_bytes"], 64 << 10);
    let suggested = data["suggested_mem_bytes"].as_u64().unwrap();
    assert!(suggested > 64 << 10, "{budget}");
    assert_eq!(
        data["suggested_mem"],
        rift_core::Bytes(suggested).to_mem_arg()
    );
    let sum = c.ok("summary", json!({"session": fs}));
    assert_eq!(sum, json!({"state": "failed", "error": budget}));
    let not_ready = c.err("segments", json!({"session": fs}), NOT_READY);
    assert_eq!(
        not_ready["data"],
        json!({"session": fs, "state": "failed", "error": budget})
    );
    let retry = c.ok(
        "open",
        json!({"a": small_a, "b": small_b, "mem": data["suggested_mem"]}),
    );
    assert!(session(&retry) != fs && retry["reused"] == false);
    c.err("summary", json!({"session": fs}), UNKNOWN_SESSION);
    c.state_of(session(&retry));

    // A slow scan: not_ready with progress, the same session for the same
    // pair, and close stops it.
    let opened = c.ok("open", json!({"a": slow, "b": pb}));
    let ss = session(&opened);
    c.until("scan progress", |c| {
        c.notes_of("progress", ss)
            .iter()
            .any(|p| p["phase"] == "scan")
    });
    let scanning = c.err("hunk", json!({"session": ss, "segment": 1}), NOT_READY);
    assert_eq!(scanning["data"]["state"], "scanning");
    assert_eq!(scanning["data"]["progress"]["phase"], "scan");
    let scan_sum = c.ok("summary", json!({"session": ss}));
    assert_eq!(scan_sum["state"], "scanning");
    assert_eq!(scan_sum["a"]["bytes"], 1u64 << 30);
    let again = c.ok("open", json!({"a": slow, "b": pb, "mem": "1G"}));
    assert_eq!(
        again,
        json!({"session": ss, "reused": true, "mem_bytes": 256u64 << 20, "state": "scanning"})
    );
    let asked = Instant::now();
    assert_eq!(
        c.ok("close", json!({"session": ss})),
        json!({"closed": true})
    );
    assert!(asked.elapsed() < Duration::from_secs(1));
    c.err("summary", json!({"session": ss}), UNKNOWN_SESSION);
    let heard = c.notes.len();
    let quiet = Instant::now();
    while quiet.elapsed() < Duration::from_millis(1200) {
        c.pump(Duration::from_millis(100));
    }
    assert!(
        !c.notes[heard..]
            .iter()
            .any(|(_, n)| n["params"]["session"] == ss),
        "nothing after close"
    );

    // A cancel that comes after the answer does nothing.
    c.send(
        &json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": 1}}),
        false,
    );

    let opened = c.ok("open", json!({"a": pa, "b": pb, "mem": "256M"}));
    assert_eq!(
        opened,
        json!({"session": session(&opened), "reused": false, "mem_bytes": 256u64 << 20, "state": "scanning"})
    );
    let s = session(&opened);
    assert_eq!(c.state_of(s)["state"], "ready");
    let mut reused = opened.clone();
    reused["reused"] = json!(true);
    reused["state"] = json!("ready");
    assert_eq!(c.ok("open", json!({"a": pa, "b": pb})), reused);
    let scans: Vec<Value> = c
        .notes_of("progress", s)
        .into_iter()
        .filter(|p| p["phase"] == "scan")
        .collect();
    assert_eq!(
        scans.last().unwrap()["done"],
        scans.last().unwrap()["total"]
    );

    let sum = c.ok("summary", json!({"session": s}));
    assert_eq!(sum["state"], "ready");
    let differing = sum["differing_segments"].as_u64().unwrap();
    assert!(differing >= 3, "{sum}");

    // Paging.
    let p1 = c.ok("segments", json!({"session": s, "offset": 0, "limit": 2}));
    let p2 = c.ok("segments", json!({"session": s, "offset": 2, "limit": 100}));
    let all: Vec<Value> = [&p1, &p2]
        .iter()
        .flat_map(|p| p["segments"].as_array().unwrap().clone())
        .collect();
    assert_eq!(all.len() as u64, p1["total"].as_u64().unwrap());

    // Hunks: a cut line and a lossy one.
    let long_at = 46;
    let long = c.ok("hunk", json!({"session": s, "line_a": long_at}));
    let cut = long["a"]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l.get("truncated").is_some())
        .expect("a cut line")
        .clone();
    assert_eq!(cut["bytes"], 10_000);
    assert_eq!(cut["text"].as_str().unwrap().len(), 4096);
    assert!(
        long["b"]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["lossy"] == true && l["text"] == "caf\u{fffd}"),
        "{long}"
    );
    let narrow = c.ok(
        "hunk",
        json!({"session": s, "line_a": long_at, "max_line_bytes": 10}),
    );
    assert!(
        narrow["a"]["lines"]
            .as_array()
            .unwrap()
            .iter()
            .all(|l| { l["text"].as_str().unwrap().len() <= 10 })
    );

    // A segment longer than max_lines is cut, and says so.
    let inserted = all
        .iter()
        .find(|x| x["b"]["count"].as_u64() > Some(300))
        .unwrap();
    let big = c.ok(
        "hunk",
        json!({"session": s, "segment": inserted["index"], "max_lines": 50}),
    );
    assert_eq!(big["truncated"], true);
    assert_eq!(big["b"]["lines"].as_array().unwrap().len(), 50);
    assert!(big["b"]["total"].as_u64() > Some(300));
    let few = c.ok(
        "hunk",
        json!({"session": s, "segment": inserted["index"], "max_lines": 4, "context": 0}),
    );
    assert_eq!(few["b"]["lines"].as_array().unwrap().len(), 4);
    assert_eq!(few["b"]["start"], few["range"]["b"]["start"]);

    // Moved lines on both sides.
    let moves = c.ok("moves", json!({"session": s}));
    assert_eq!(moves["total"], 1, "{moves}");
    let block_at = &moves["blocks"][0];
    assert_eq!(block_at["a"], json!({"start": 21, "count": 5}));
    let on_a = c.ok("hunk", json!({"session": s, "line_a": 22}));
    let moved_lines = |col: &Value| {
        col["lines"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|l| l["moved"] == true)
            .count()
    };
    assert_eq!(moved_lines(&on_a["a"]), 5, "{on_a}");
    let b_line = block_at["b"]["start"].as_u64().unwrap();
    let last = c.ok(
        "hunk",
        json!({"session": s, "segment": all.last().unwrap()["index"], "max_lines": 5000}),
    );
    assert!(moved_lines(&last["b"]) == 5, "{last} {b_line}");

    // An equal segment reached by line starts at that line.
    let equal = c.ok(
        "hunk",
        json!({"session": s, "line_a": 30, "context": 1, "max_lines": 2}),
    );
    assert_eq!(equal["status"], "equal");
    assert_eq!(equal["a"]["start"], 29);
    assert_eq!(
        equal["a"]["lines"][1]["text"],
        equal["b"]["lines"][1]["text"]
    );
    // Line 30 of A: lines 1 to 20, the five moved rows, then 21 on.
    assert_eq!(equal["a"]["lines"][1]["text"], "line 25");

    let e = c.err("hunk", json!({"session": s, "segment": 99}), INVALID_PARAMS);
    assert_eq!(e["data"]["param"], "segment");
    assert_eq!(e["data"]["value"], 99);
    let e = c.err(
        "hunk",
        json!({"session": s, "line_a": 99_999}),
        INVALID_PARAMS,
    );
    assert_eq!(e["data"]["param"], "line_a");
    assert_eq!(e["data"]["value"], 99_999);
    assert_eq!(e["data"]["limit"], 66);
    c.err_bad(
        "hunk",
        json!({"session": s, "segment": 1, "line_a": 3}),
        INVALID_PARAMS,
    );

    assert_eq!(
        c.ok(
            "set_visible",
            json!({"session": s, "a": {"start": 8, "count": 5}})
        ),
        Value::Null
    );

    // Malformed input gets an error, not a crash.
    c.send_body(b"{not json");
    c.until("parse error", |c| !c.nulls.is_empty());
    assert_eq!(c.nulls.pop().unwrap()["error"]["code"], PARSE_ERROR);
    c.send_body(
        json!([{"jsonrpc": "2.0", "id": 99, "method": "summary"}])
            .to_string()
            .as_bytes(),
    );
    c.until("batch refusal", |c| !c.nulls.is_empty());
    assert_eq!(c.nulls.pop().unwrap()["error"]["code"], INVALID_REQUEST);

    assert_eq!(
        c.ok("close", json!({"session": s})),
        json!({"closed": true})
    );
    assert_eq!(
        c.ok("close", json!({"session": s})),
        json!({"closed": false})
    );
    let unknown = c.err("summary", json!({"session": s}), UNKNOWN_SESSION);
    assert_eq!(unknown["data"], json!({"session": s}));
    let reopened = c.ok("open", json!({"a": pa, "b": pb}));
    assert!(session(&reopened) > s && reopened["reused"] == false);
    clean_exit(c);
}

#[test]
fn malformed_input_gets_errors_and_the_server_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a", &numbered("line", 0..100));
    let mut c = Client::start(&[]);
    c.send_body(b"{not json");
    c.send_body(b"\xff\xfe");
    c.send_body(b"[]");
    c.send_body(b"42");
    c.send_body(br#"{"jsonrpc":"1.0","id":1005,"method":"summary"}"#);
    c.send_body(br#"{"jsonrpc":"2.0","id":{"x":1},"method":"summary"}"#);
    c.send_body(br#"{"jsonrpc":"2.0","id":1006}"#);
    // No Content-Length: a header block with none, then a message again.
    c.send_raw(b"Content-Type: text/plain\r\n\r\n");
    // A body over the limit is skipped, not read.
    let big = vec![b' '; 1 << 20];
    c.send_body(&big);
    c.send_body(br#"{"jsonrpc":"2.0","id":1007,"method":7}"#);
    c.until("the refusals", |c| c.nulls.len() >= 7 && c.answered(1007));
    let codes: Vec<i64> = c
        .nulls
        .iter()
        .map(|m| m["error"]["code"].as_i64().unwrap())
        .collect();
    assert_eq!(
        codes,
        [
            PARSE_ERROR,
            PARSE_ERROR,
            INVALID_REQUEST,
            INVALID_REQUEST,
            INVALID_REQUEST,
            PARSE_ERROR,
            INVALID_REQUEST
        ]
    );
    // With an id the refusal names it.
    for id in [1005, 1006, 1007] {
        assert_eq!(c.answer(id)["error"]["code"], INVALID_REQUEST);
    }

    c.initialize();
    c.err_bad("open", json!(["a", "b"]), INVALID_PARAMS);
    c.err_bad("open", json!({"a": a}), INVALID_PARAMS);
    c.err_bad("open", json!({"a": a, "b": a, "extra": 1}), INVALID_PARAMS);
    c.err_bad("open", json!({"a": 1, "b": a}), INVALID_PARAMS);
    c.err_bad("open", json!({"a": "", "b": a}), INVALID_PARAMS);
    c.err_bad("summary", json!({"session": -1}), INVALID_PARAMS);
    c.err_bad("summary", json!({"session": 1.5}), INVALID_PARAMS);
    c.err_bad("summary", json!({}), INVALID_PARAMS);
    c.err_bad(
        "segments",
        json!({"session": 1, "limit": 0}),
        INVALID_PARAMS,
    );
    c.err_bad("hunk", json!({"session": 1, "line_a": 0}), INVALID_PARAMS);
    c.err_bad("hunk", json!({"session": 1}), INVALID_PARAMS);
    c.err_bad(
        "set_visible",
        json!({"session": 1, "a": {"start": 0, "count": 1}}),
        INVALID_PARAMS,
    );
    c.err_bad("set_visible", json!({"session": 1}), INVALID_PARAMS);
    c.err("open", json!({"a": a, "b": a, "mem": "0"}), INVALID_PARAMS);
    c.err("summary", json!({"session": 1}), UNKNOWN_SESSION);
    // Unknown notifications are ignored.
    c.send(&json!({"jsonrpc": "2.0", "method": "$/whatever"}), false);
    let s = open_ready(&mut c, &a, &a, "16M");
    assert_eq!(c.ok("summary", json!({"session": s}))["identical"], true);
    clean_exit(c);
}

#[test]
fn a_cut_message_at_the_end_of_input_ends_the_server_cleanly() {
    let mut c = Client::start(&[]);
    c.initialize();
    c.send_raw(b"Content-Length: 100\r\n\r\n{\"jsonrpc\"");
    clean_exit(c);
}

#[test]
fn a_huge_single_line_is_cut_and_never_held_whole() {
    let dir = tempfile::tempdir().unwrap();
    let mut line = vec![b'y'; 64 << 20];
    line.push(b'\n');
    let a = write(dir.path(), "a", &line);
    line[0] = b'z';
    line.extend(b"tail\n");
    let b = write(dir.path(), "b", &line);
    drop(line);
    let mut c = Client::start(&[
        "--stats",
        "--max-total-mem",
        "64M",
        "--max-session-mem",
        "32M",
    ]);
    c.initialize();
    let s = open_ready(&mut c, &a, &b, "32M");
    let h = c.ok(
        "hunk",
        json!({"session": s, "segment": 0, "max_line_bytes": 65536}),
    );
    let first = &h["a"]["lines"][0];
    assert_eq!(first["bytes"], 64 << 20);
    assert_eq!(first["truncated"], true);
    assert!(first["text"].as_str().unwrap().len() <= 65536);
    assert_eq!(h["b"]["lines"][1]["text"], "tail");
    let stderr = clean_exit(c);
    let peak = peak_heap(&stderr);
    assert!(peak <= 64 << 20, "peak heap {peak} over --max-total-mem");
}

#[test]
fn a_cancelled_request_is_answered_once() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a", &numbered("line", 0..2000));
    let b = write(dir.path(), "b", &numbered("other", 0..2000));
    let mut c = Client::start(&[]);
    c.initialize();
    let s = open_ready(&mut c, &a, &b, "16M");
    let ids: Vec<u64> = (0..20)
        .map(|_| {
            let id = c.request(
                "hunk",
                &json!({"session": s, "segment": 0, "max_lines": 5000}),
            );
            c.send(
                &json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": id}}),
                false,
            );
            id
        })
        .collect();
    let mut cancelled = 0;
    for id in ids {
        let msg = c.answer(id);
        let was = msg["error"]["code"] == -32800;
        // Hunks that run together may find the response pool held.
        let busy = msg["error"]["code"] == -32005;
        assert!(was || busy || msg.get("result").is_some(), "{msg}");
        cancelled += usize::from(was);
    }
    eprintln!("{cancelled} of 20 requests cancelled");
    // finish() checks that every id was answered exactly once.
    clean_exit(c);
}

#[test]
fn closing_input_during_a_scan_exits_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let slow = huge(dir.path());
    let b = write(dir.path(), "b", b"x\n");
    let mut c = Client::start(&[]);
    c.initialize();
    c.ok("open", json!({"a": slow, "b": b}));
    let began = Instant::now();
    clean_exit(c);
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "{:?}",
        began.elapsed()
    );
}

#[test]
fn the_whole_heap_stays_under_max_total_mem() {
    let dir = tempfile::tempdir().unwrap();
    let (gen_a, gen_b) = generated(dir.path(), 16 << 20);
    let mut c = Client::start(&[
        "--stats",
        "--max-total-mem",
        "24M",
        "--max-session-mem",
        "16M",
    ]);
    c.initialize();
    let s = open_ready(
        &mut c,
        gen_a.to_str().unwrap(),
        gen_b.to_str().unwrap(),
        "16M",
    );
    c.ok("summary", json!({"session": s}));
    let page = c.ok("segments", json!({"session": s, "limit": 1000}));
    c.ok("moves", json!({"session": s, "limit": 1000}));
    let big = json!({"max_lines": 5000, "max_line_bytes": 65536, "context": 100});
    let ids: Vec<u64> = page["segments"]
        .as_array()
        .unwrap()
        .iter()
        .take(40)
        .map(|seg| {
            let mut p = big.clone();
            p["session"] = json!(s);
            p["segment"] = seg["index"].clone();
            c.request("hunk", &p)
        })
        .collect();
    for id in ids {
        let msg = c.answer(id);
        let busy = msg["error"]["code"] == -32005;
        assert!(busy || msg.get("result").is_some(), "{msg}");
    }
    let stderr = clean_exit(c);
    let peak = peak_heap(&stderr);
    assert!(peak <= 24 << 20, "peak heap {peak} over --max-total-mem");
}
