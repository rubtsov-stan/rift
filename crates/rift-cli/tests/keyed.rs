//! `rift --key` end to end.

use std::path::Path;
use std::process::{Command, Output};

fn rift_on(a: &[u8], b: &[u8], args: &[&str]) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let (pa, pb) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::write(&pa, a).unwrap();
    std::fs::write(&pb, b).unwrap();
    rift_paths(&pa, &pb, args)
}

fn rift_paths(a: &Path, b: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rift"))
        .args(args)
        .arg(a)
        .arg(b)
        .output()
        .unwrap()
}

fn rift(args: &[&str]) -> Output {
    rift_on(b"id,v\n1,x\n", b"id,v\n1,y\n", args)
}

#[test]
fn a_changed_record_prints_its_key_and_both_lines() {
    let out = rift(&["--key", "csv:id"]);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap(), "~1\t2\t2\n");
    assert_eq!(out.stderr, b"");
}

#[test]
fn reordered_rows_are_identical() {
    let out = rift_on(
        b"{\"id\":1,\"v\":2}\n{\"id\":2}\n",
        b"{\"id\":2}\r\n{\"id\":1,\"v\":2}\n",
        &["--key", "json:id"],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout, b"");
}

#[test]
fn stats_go_to_standard_error() {
    let out = rift(&["--key", "csv:1", "--stats"]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("peak heap"), "{stderr}");
    assert!(
        stderr.contains("passes 1, records 2 in a and 2 in b"),
        "{stderr}"
    );
}

#[test]
fn duplicate_keys_exit_two() {
    let out = rift_on(b"1,a\n2,b\n1,c\n", b"1,a\n", &["--key", "csv:1"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "rift: duplicate keys: 1\n"
    );
    assert_eq!(out.stdout, b"");
}

#[test]
fn a_missing_column_exits_two() {
    let out = rift(&["--key", "csv:name"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "rift: a has no column \"name\"\n"
    );
}

#[test]
fn a_malformed_key_is_a_usage_error() {
    let out = rift(&["--key", "id"]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(
        stderr.contains("must start with csv:, json: or copy:"),
        "{stderr}"
    );
}

#[test]
fn key_mode_does_not_combine_with_summary() {
    let out = rift(&["--key", "csv:1", "--summary"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be used with"));
}
