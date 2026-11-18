//! The key mode tools through an in-process client, on small files.

use std::path::Path;

use rift_core::Bytes;
use rift_mcp::Server;
use rift_mcp::keyed::{
    MAX_CHANGES, MAX_FIELD_NAMES, MAX_FIELDS, MAX_RECORD_BYTES, MAX_VALUE_BYTES,
};
use rift_mcp::render::MAX_TEXT_BYTES;
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

/// The message of a tool error.
async fn err(c: &Client, tool: &'static str, args: Value) -> String {
    let r = call(c, tool, args).await;
    assert_eq!(r.is_error, Some(true), "{r:?}");
    serde_json::to_value(&r.content[0]).unwrap()["text"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn write(dir: &Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    path.to_str().unwrap().to_owned()
}

const CSV_A: &str = "id,name,price\n1,apple,10\n2,pear,20\n3,plum,30\n";
const CSV_B: &str = "id,name,price\n1,apple,12\n3,plum,30\n4,fig,5\n";

fn csv(dir: &Path) -> Value {
    json!({"a": write(dir, "a.csv", CSV_A), "b": write(dir, "b.csv", CSV_B), "key": "csv:id"})
}

fn keys(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|k| k["key"].as_str().unwrap().to_owned())
        .collect()
}

fn with(args: &Value, more: Value) -> Value {
    let mut args = args.clone();
    let Value::Object(more) = more else {
        panic!("not an object")
    };
    args.as_object_mut().unwrap().extend(more);
    args
}

#[tokio::test]
async fn the_summary_counts_keys_and_lists_the_first_of_each_kind() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::default();
    let c = connect(&server).await;
    let s = ok(&c, "keyed_summary", csv(dir.path())).await;
    assert_eq!(
        (s["a_records"].as_u64(), s["b_records"].as_u64()),
        (Some(4), Some(4)),
        "the header row counts as a record"
    );
    assert_eq!(
        (
            s["added"].as_u64(),
            s["removed"].as_u64(),
            s["changed"].as_u64()
        ),
        (Some(1), Some(1), Some(1))
    );
    assert_eq!(keys(&s["first_added"]), ["4"]);
    assert_eq!(keys(&s["first_removed"]), ["2"]);
    assert_eq!(keys(&s["first_changed"]), ["1"]);
    assert_eq!(s["first_changed"][0]["a_line"], 2);
    assert_eq!(s["first_changed"][0]["b_line"], 2);
    assert_eq!(s["first_removed"][0]["b_line"], Value::Null);
    assert_eq!(s["first_added"][0]["a_line"], Value::Null);
    assert_eq!(s["first_added"][0]["table"], Value::Null);
    assert_eq!(s["mem_budget_bytes"], Bytes::mib(64).0);
    // A keyed run is not a session, and gives its room back.
    assert_eq!(server.sessions().len(), 0);
    assert_eq!(server.sessions().mem(), Bytes(0));
}

#[tokio::test]
async fn the_summary_lists_ten_keys_per_kind_and_counts_all() {
    let dir = tempfile::tempdir().unwrap();
    let a: String = std::iter::once("id,v\n".to_owned())
        .chain((0..25).map(|i| format!("{i},x\n")))
        .collect();
    let b: String = std::iter::once("id,v\n".to_owned())
        .chain((0..25).map(|i| format!("{i},y\n")))
        .collect();
    let args =
        json!({"a": write(dir.path(), "a", &a), "b": write(dir.path(), "b", &b), "key": "csv:1"});
    let c = connect(&Server::default()).await;
    let s = ok(&c, "keyed_summary", args).await;
    assert_eq!(s["changed"], 25);
    assert_eq!(s["first_changed"].as_array().unwrap().len(), 10);
}

const COPY_A: &str = "COPY public.item (id, v) FROM stdin;\n1\tx\n2\tx\n\\.\n\
                      COPY \"public\".\"a:b\" (id, v) FROM stdin;\n1\tx\n\\.\n";
const COPY_B: &str = "COPY public.item (id, v) FROM stdin;\n1\ty\n3\tx\n\\.\n\
                      COPY \"public\".\"a:b\" (id, v) FROM stdin;\n1\ty\n\\.\n";

fn copy(dir: &Path) -> Value {
    json!({"a": write(dir, "a.sql", COPY_A), "b": write(dir, "b.sql", COPY_B), "key": "copy:id"})
}

#[tokio::test]
async fn copy_keys_carry_their_table_as_a_field() {
    let dir = tempfile::tempdir().unwrap();
    let c = connect(&Server::default()).await;
    let s = ok(&c, "keyed_summary", copy(dir.path())).await;
    let tables = |list: &Value| -> Vec<(String, String)> {
        let mut v: Vec<_> = list
            .as_array()
            .unwrap()
            .iter()
            .map(|k| {
                (
                    k["table"].as_str().unwrap().to_owned(),
                    k["key"].as_str().unwrap().to_owned(),
                )
            })
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        tables(&s["first_changed"]),
        [
            ("\"public\".\"a:b\"".to_owned(), "1".to_owned()),
            ("public.item".to_owned(), "1".to_owned())
        ]
    );
    assert_eq!(
        tables(&s["first_added"]),
        [("public.item".into(), "3".into())]
    );
    let p = ok(&c, "keyed_changes", copy(dir.path())).await;
    for e in p["changes"].as_array().unwrap() {
        assert!(e["table"].as_str().unwrap().contains("public"), "{e}");
        assert!(!e["key"].as_str().unwrap().contains(':'), "{e}");
    }
}

#[tokio::test]
async fn the_changes_carry_records_and_changed_fields() {
    let dir = tempfile::tempdir().unwrap();
    let c = connect(&Server::default()).await;
    let p = ok(&c, "keyed_changes", csv(dir.path())).await;
    assert_eq!(
        (p["total"].as_u64(), p["next"].clone()),
        (Some(3), Value::Null)
    );
    let by_kind = |kind: &str| -> Value {
        p["changes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["kind"] == kind)
            .unwrap()
            .clone()
    };
    let changed = by_kind("changed");
    assert_eq!(changed["key"], "1");
    assert_eq!(changed["a_record"], "1,apple,10");
    assert_eq!(changed["b_record"], "1,apple,12");
    assert_eq!(
        changed["fields"],
        json!([{"name": "price", "a": "10", "b": "12", "truncated": false}])
    );
    let added = by_kind("added");
    assert_eq!(added["a_record"], Value::Null);
    assert_eq!(added["b_record"], "4,fig,5");
    assert_eq!(added["fields"], Value::Null);
    let removed = by_kind("removed");
    assert_eq!(removed["a_record"], "2,pear,20");
    assert_eq!(removed["b_record"], Value::Null);
}

#[tokio::test]
async fn fields_limit_the_comparison_to_the_named_ones() {
    let dir = tempfile::tempdir().unwrap();
    let c = connect(&Server::default()).await;
    let p = ok(
        &c,
        "keyed_changes",
        with(&csv(dir.path()), json!({"fields": ["name"]})),
    )
    .await;
    // Key 1 changed only in price: it drops out. Added and removed stay.
    assert_eq!(
        (p["total"].as_u64(), p["changed"].as_u64()),
        (Some(2), Some(0))
    );
}

fn many(dir: &Path, n: usize) -> Value {
    let rows = |v: &str| -> String {
        std::iter::once("id,v\n".to_owned())
            .chain((0..n).map(|i| format!("{i},{v}\n")))
            .collect()
    };
    json!({"a": write(dir, "a", &rows("a")), "b": write(dir, "b", &rows("b")), "key": "csv:id"})
}

#[tokio::test]
async fn pages_cover_every_change_once_and_limit_is_clamped() {
    let dir = tempfile::tempdir().unwrap();
    let c = connect(&Server::default()).await;
    let args = many(dir.path(), 250);
    let first = ok(&c, "keyed_changes", args.clone()).await;
    assert_eq!(
        first["changes"].as_array().unwrap().len(),
        20,
        "default limit"
    );
    assert!(first["next"].is_string());
    let big = ok(&c, "keyed_changes", with(&args, json!({"limit": 100_000}))).await;
    assert_eq!(big["changes"].as_array().unwrap().len() as u64, MAX_CHANGES);
    let (mut seen, mut cursor, mut pages) = (Vec::new(), None::<String>, 0);
    loop {
        let mut call = with(&args, json!({"limit": 100}));
        if let Some(cursor) = &cursor {
            call = with(&call, json!({"cursor": cursor}));
        }
        let p = ok(&c, "keyed_changes", call).await;
        assert_eq!(p["total"], 250);
        pages += 1;
        seen.extend(
            p["changes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["key"].as_str().unwrap().to_owned()),
        );
        match p["next"].as_str() {
            Some(next) => cursor = Some(next.to_owned()),
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen.len(), 250);
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), 250, "a change came on two pages");
    let zero = ok(&c, "keyed_changes", with(&args, json!({"limit": 0}))).await;
    assert_eq!(
        zero["changes"].as_array().unwrap().len(),
        1,
        "limit is at least 1"
    );
}

#[tokio::test]
async fn long_text_is_cut_and_a_page_stops_at_the_text_bound() {
    let dir = tempfile::tempdir().unwrap();
    let c = connect(&Server::default()).await;
    let cols = 40;
    let head: String = (0..cols)
        .map(|i| format!("c{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let rows = |v: char| -> String {
        let mut s = format!("{head}\n");
        for r in 0..60 {
            let long = v.to_string().repeat(MAX_VALUE_BYTES + 50);
            let cells: Vec<String> = (0..cols)
                .map(|i| {
                    if i == 0 {
                        format!("k{r}")
                    } else {
                        long.clone()
                    }
                })
                .collect();
            s += &(cells.join(",") + "\n");
        }
        s
    };
    let args = json!({"a": write(dir.path(), "a", &rows('a')), "b": write(dir.path(), "b", &rows('b')), "key": "csv:c0"});
    let p = ok(&c, "keyed_changes", with(&args, json!({"limit": 100}))).await;
    let changes = p["changes"].as_array().unwrap();
    let text: usize = changes
        .iter()
        .map(|e| {
            let s = |v: &Value| v.as_str().map_or(0, str::len);
            let fields: usize = e["fields"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| s(&f["name"]) + s(&f["a"]) + s(&f["b"]))
                .sum();
            s(&e["key"]) + s(&e["a_record"]) + s(&e["b_record"]) + fields
        })
        .sum();
    assert!(text <= MAX_TEXT_BYTES, "{text} bytes of text");
    assert!(changes.len() < 60, "the text bound stopped the page");
    assert!(p["next"].is_string(), "the bound ended the page");
    let e = &changes[0];
    assert_eq!(e["records_truncated"], true);
    assert!(e["a_record"].as_str().unwrap().len() <= MAX_RECORD_BYTES);
    let fields = e["fields"].as_array().unwrap();
    assert_eq!(fields.len(), MAX_FIELDS);
    assert_eq!(e["fields_more"].as_u64(), Some(39 - MAX_FIELDS as u64));
    assert_eq!(fields[0]["truncated"], true);
    assert_eq!(fields[0]["a"].as_str().unwrap().len(), MAX_VALUE_BYTES);
}

#[tokio::test]
async fn a_long_key_is_cut_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let key = "é".repeat(300);
    let old = write(dir.path(), "a", &format!("id,v\n{key},1\n"));
    let new = write(dir.path(), "b", &format!("id,v\n{key},2\n"));
    let c = connect(&Server::default()).await;
    let s = ok(
        &c,
        "keyed_summary",
        json!({"a": old, "b": new, "key": "csv:id"}),
    )
    .await;
    let first = &s["first_changed"][0];
    assert_eq!(first["key_truncated"], true);
    assert_eq!(first["key"], "é".repeat(MAX_VALUE_BYTES / 2));
}

#[tokio::test]
async fn errors_come_back_as_tool_errors_in_the_cli_wording() {
    let dir = tempfile::tempdir().unwrap();
    let args = csv(dir.path());
    let c = connect(&Server::default()).await;
    let a = args["a"].as_str().unwrap().to_owned();
    for tool in ["keyed_summary", "keyed_changes"] {
        let m = err(&c, tool, with(&args, json!({"key": "csv:nope"}))).await;
        assert_eq!(m, format!("a ({a}) has no column \"nope\""));
        let m = err(&c, tool, with(&args, json!({"key": "id"}))).await;
        assert_eq!(m, "key: key \"id\" must start with csv:, json: or copy:");
        let m = err(&c, tool, with(&args, json!({"key": "csv:0"}))).await;
        assert!(m.contains("numbered from 1"), "{m}");
        let m = err(&c, tool, with(&args, json!({"mem": "257M"}))).await;
        assert!(m.contains("over the limit"), "{m}");
        let m = err(&c, tool, with(&args, json!({"mem": "lots"}))).await;
        assert!(m.starts_with("mem: "), "{m}");
        let m = err(&c, tool, with(&args, json!({"a": dir.path().join("none")}))).await;
        assert!(m.starts_with("a: "), "{m}");
    }
}

#[tokio::test]
async fn duplicate_keys_fail_the_call_and_name_their_table() {
    let dir = tempfile::tempdir().unwrap();
    let c = connect(&Server::default()).await;
    let dup = "COPY public.item (id, v) FROM stdin;\n1\tx\n1\ty\n\\.\n";
    let args = json!({"a": write(dir.path(), "a", dup), "b": write(dir.path(), "b", COPY_B), "key": "copy:id"});
    for tool in ["keyed_summary", "keyed_changes"] {
        let m = err(&c, tool, args.clone()).await;
        assert_eq!(m, "duplicate keys: public.item:1", "{tool}");
    }
    let dup = write(dir.path(), "d.csv", "id,v\n7,x\n7,y\n");
    let m = err(
        &c,
        "keyed_summary",
        json!({"a": dup, "b": dup, "key": "csv:id"}),
    )
    .await;
    assert_eq!(m, "duplicate keys: 7");
}

#[tokio::test]
async fn a_budget_refusal_names_a_mem_to_rerun_with() {
    let dir = tempfile::tempdir().unwrap();
    let args = with(&csv(dir.path()), json!({"mem": "100K"}));
    let c = connect(&Server::default()).await;
    for tool in ["keyed_summary", "keyed_changes"] {
        let m = err(&c, tool, args.clone()).await;
        assert!(m.contains("budget exceeded"), "{m}");
        // Following the advice gets there: each rerun asks for more.
        let mut mem = String::from("100K");
        let mut m = m;
        for _ in 0..8 {
            let Some((_, advice)) = m.rsplit_once("; rerun with --mem ") else {
                panic!("{m}");
            };
            mem = advice.to_owned();
            let r = call(&c, tool, with(&args, json!({"mem": mem}))).await;
            if r.is_error != Some(true) {
                break;
            }
            m = serde_json::to_value(&r.content[0]).unwrap()["text"]
                .as_str()
                .unwrap()
                .to_owned();
        }
        let r = call(&c, tool, with(&args, json!({"mem": mem}))).await;
        assert_ne!(r.is_error, Some(true), "{tool} with {mem}: {r:?}");
    }
}

#[tokio::test]
async fn a_run_holds_its_budget_from_the_servers_total() {
    let dir = tempfile::tempdir().unwrap();
    let server = Server::new(Limits {
        total_mem: Bytes::mib(64),
        wait: std::time::Duration::ZERO,
        ..Limits::default()
    });
    let c = connect(&server).await;
    let lease = server.sessions().lease(Some(Bytes::mib(64))).unwrap();
    let m = err(&c, "keyed_summary", csv(dir.path())).await;
    assert!(m.contains("the server is busy"), "{m}");
    drop(lease);
    ok(&c, "keyed_summary", csv(dir.path())).await;
    assert_eq!(server.sessions().mem(), Bytes(0));
}

#[tokio::test]
async fn escaped_text_counts_against_the_text_bound() {
    let dir = tempfile::tempdir().unwrap();
    let c = connect(&Server::default()).await;
    let rows = |v: char| -> String {
        let junk = format!("{v}{}", "\u{1}".repeat(900));
        std::iter::once("id,v\n".to_owned())
            .chain((0..100).map(|r| format!("{r},{junk}\n")))
            .collect()
    };
    let args = json!({"a": write(dir.path(), "a", &rows('a')), "b": write(dir.path(), "b", &rows('b')), "key": "csv:id"});
    let p = ok(&c, "keyed_changes", with(&args, json!({"limit": 100}))).await;
    let json_len = serde_json::to_string(&p["changes"]).unwrap().len();
    assert!(json_len <= MAX_TEXT_BYTES, "{json_len} bytes of JSON");
    assert!(p["next"].is_string(), "the bound ended the page");
}

#[tokio::test]
async fn a_cut_does_not_split_a_character_and_empty_fields_mean_all() {
    let dir = tempfile::tempdir().unwrap();
    let c = connect(&Server::default()).await;
    let key = format!("a{}", "é".repeat(200));
    let old = write(dir.path(), "a", &format!("id,v\n{key},1\n"));
    let new = write(dir.path(), "b", &format!("id,v\n{key},2\n"));
    let args = json!({"a": old, "b": new, "key": "csv:id", "fields": []});
    let p = ok(&c, "keyed_changes", args).await;
    assert_eq!(p["changed"], 1, "an empty list compares every field");
    let shown = p["changes"][0]["key"].as_str().unwrap();
    assert_eq!(shown, format!("a{}", "é".repeat(127)));
}

/// The first page of `args` at `limit` 2, and its cursor.
async fn first_cursor(c: &Client, args: &Value) -> String {
    let p = ok(c, "keyed_changes", with(args, json!({"limit": 2}))).await;
    p["next"].as_str().expect("a next page").to_owned()
}

const CHANGED: &str = "changed since that cursor was issued";

#[tokio::test]
async fn a_cursor_of_this_call_gives_the_next_page() {
    let dir = tempfile::tempdir().unwrap();
    let c = connect(&Server::default()).await;
    let args = many(dir.path(), 5);
    let cursor = first_cursor(&c, &args).await;
    let p = ok(&c, "keyed_changes", with(&args, json!({"cursor": cursor}))).await;
    assert_eq!(
        p["changes"].as_array().unwrap().len(),
        3,
        "5 changes, 2 seen"
    );
    assert_eq!(p["next"], Value::Null);
}

#[tokio::test]
async fn a_cursor_is_refused_after_a_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let c = connect(&Server::default()).await;
    let args = many(dir.path(), 5);
    let b = args["b"].as_str().unwrap().to_owned();
    // Same size and bytes, later mtime.
    let cursor = first_cursor(&c, &args).await;
    let file = std::fs::OpenOptions::new().write(true).open(&b).unwrap();
    file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
        .unwrap();
    drop(file);
    let m = err(&c, "keyed_changes", with(&args, json!({"cursor": cursor}))).await;
    assert!(m.contains(CHANGED) && m.contains("without a cursor"), "{m}");
    // A new size.
    let cursor = first_cursor(&c, &args).await;
    let mut text = std::fs::read_to_string(&b).unwrap();
    text.push_str("99,z\n");
    std::fs::write(&b, text).unwrap();
    let m = err(&c, "keyed_changes", with(&args, json!({"cursor": cursor}))).await;
    assert!(m.contains(CHANGED), "{m}");
    // The other file, replaced by a copy: same bytes, another inode.
    let cursor = first_cursor(&c, &args).await;
    let a = args["a"].as_str().unwrap();
    let bytes = std::fs::read(a).unwrap();
    std::fs::remove_file(a).unwrap();
    std::fs::write(a, bytes).unwrap();
    let m = err(&c, "keyed_changes", with(&args, json!({"cursor": cursor}))).await;
    assert!(m.contains(CHANGED), "{m}");
}

#[tokio::test]
async fn a_cursor_is_refused_after_the_key_mem_or_fields_change() {
    let dir = tempfile::tempdir().unwrap();
    let c = connect(&Server::default()).await;
    let args = many(dir.path(), 5);
    let cursor = first_cursor(&c, &args).await;
    for other in [
        json!({"key": "csv:1"}),
        json!({"mem": "32M"}),
        json!({"fields": ["v"]}),
    ] {
        let call = with(&with(&args, other.clone()), json!({"cursor": cursor}));
        let m = err(&c, "keyed_changes", call).await;
        assert!(m.contains(CHANGED), "{other}: {m}");
    }
    // `limit` is not part of it.
    let call = with(&args, json!({"cursor": cursor, "limit": 1}));
    ok(&c, "keyed_changes", call).await;
}

#[tokio::test]
async fn a_cursor_of_another_server_run_pair_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let c = connect(&Server::default()).await;
    let one = many(dir.path(), 5);
    let cursor = first_cursor(&c, &one).await;
    let other = tempfile::tempdir().unwrap();
    let two = many(other.path(), 5);
    let m = err(&c, "keyed_changes", with(&two, json!({"cursor": cursor}))).await;
    assert!(m.contains(CHANGED), "{m}");
}

#[tokio::test]
async fn garbage_cursors_are_refused_without_a_panic() {
    let dir = tempfile::tempdir().unwrap();
    let c = connect(&Server::default()).await;
    let args = many(dir.path(), 5);
    let good = first_cursor(&c, &args).await;
    let mut flipped = good.clone().into_bytes();
    let at = 5;
    flipped[at] = if flipped[at] == b'0' { b'1' } else { b'0' };
    let bad: Vec<String> = vec![
        String::new(),
        "x".into(),
        "r1".into(),
        good[..good.len() - 1].to_owned(),
        good[..good.len() / 2].to_owned(),
        format!("{good}0"),
        format!("r2{}", &good[2..]),
        good.to_uppercase(),
        "r1".to_owned() + &"z".repeat(48),
        "\u{0}\u{ff}é".into(),
        "r".repeat(100_000),
        String::from_utf8(flipped).unwrap(),
    ];
    for cursor in bad {
        let m = err(&c, "keyed_changes", with(&args, json!({"cursor": cursor}))).await;
        assert!(m.starts_with("cursor: "), "{m}");
        assert!(m.contains("without a cursor"), "{m}");
    }
}

#[tokio::test]
async fn one_change_with_heavy_fields_still_fits_the_text_bound() {
    let dir = tempfile::tempdir().unwrap();
    let c = connect(&Server::default()).await;
    // Control characters cost six bytes each as JSON: 39 fields of names
    // and values at the cut length weigh far more than the bound.
    let junk = |tail: char| format!("{}{tail}", "\u{1}".repeat(MAX_VALUE_BYTES - 1));
    let file = |name: &str, tail: char| {
        let names: Vec<String> = (0..39).map(|i| format!("c{i:02}{}", junk('n'))).collect();
        let values: Vec<String> = (0..39).map(|_| junk(tail)).collect();
        let text = format!("id,{}\nk,{}\n", names.join(","), values.join(","));
        write(dir.path(), name, &text)
    };
    let args = json!({"a": file("a", 'x'), "b": file("b", 'y'), "key": "csv:id"});
    let p = ok(&c, "keyed_changes", args).await;
    let len = serde_json::to_string(&p["changes"]).unwrap().len();
    assert!(len <= MAX_TEXT_BYTES, "{len} bytes of JSON");
    assert!(p["changes"][0]["fields_more"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn too_many_or_too_long_field_names_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let c = connect(&Server::default()).await;
    let args = many(dir.path(), 3);
    let names: Vec<String> = (0..=MAX_FIELD_NAMES).map(|i| format!("f{i}")).collect();
    let m = err(&c, "keyed_changes", with(&args, json!({"fields": names}))).await;
    assert!(m.starts_with("fields: at most"), "{m}");
    let long = "x".repeat(MAX_VALUE_BYTES + 1);
    let m = err(&c, "keyed_changes", with(&args, json!({"fields": [long]}))).await;
    assert!(m.starts_with("fields: at most"), "{m}");
}
