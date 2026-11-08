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
    let dir = tempfile::tempdir().unwrap();
    let (pa, pb) = (dir.path().join("old.csv"), dir.path().join("new.csv"));
    std::fs::write(&pa, b"id,v\n1,x\n").unwrap();
    std::fs::write(&pb, b"id,v\n1,y\n").unwrap();
    let out = rift_paths(&pa, &pb, &["--key", "csv:name"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        format!("rift: a ({}) has no column \"name\"\n", pa.display())
    );
}

#[test]
fn a_record_without_a_key_names_its_file() {
    let dir = tempfile::tempdir().unwrap();
    let (pa, pb) = (dir.path().join("old.json"), dir.path().join("new.json"));
    std::fs::write(&pa, b"{\"id\":1}\n").unwrap();
    std::fs::write(&pb, b"{\"id\":1}\n{\"v\":2}\n").unwrap();
    let out = rift_paths(&pa, &pb, &["--key", "json:id"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        format!("rift: line 2 of b ({}) has no key\n", pb.display())
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

#[test]
fn show_records_prints_each_record_under_its_change() {
    let out = rift_on(
        b"id,v\r\n1,x\r\n2,\"two\r\nlines\"\n3,z\n",
        b"id,v\n3,z\n4,\"say \"\"hi\"\"\"\n1,X",
        &["--key", "csv:id", "--show-records"],
    );
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "~1\t2\t4\n  - 1,x\n  + 1,X\n\
         -2\t3\t-\n  - 2,\"two\r\n  - lines\"\n\
         +4\t-\t3\n  + 4,\"say \"\"hi\"\"\"\n"
    );
}

#[test]
fn show_records_needs_key() {
    let out = rift(&["--show-records"]);
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("--key <SPEC>"), "{stderr}");
}

/// Strips the record lines that follow each change line and checks them
/// against the files: the A record behind `  - `, the B one behind `  + `.
#[test]
fn show_records_matches_the_files_on_a_generated_pair() {
    let mut config = rift_gen::Config::new(7, 1 << 20, rift_gen::Profile::Shuffled);
    config.change_ratio = 0.01;
    let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
    rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
    let plain = rift_on(&a, &b, &["--key", "csv:id", "--mem", "1M"]);
    let out = rift_on(
        &a,
        &b,
        &["--key", "csv:id", "--show-records", "--mem", "1M"],
    );
    assert_eq!(out.status.code(), Some(1));
    let (la, lb): (Vec<&[u8]>, Vec<&[u8]>) = (
        a.split(|&c| c == b'\n').collect(),
        b.split(|&c| c == b'\n').collect(),
    );
    let text = String::from_utf8(out.stdout).unwrap();
    let mut changes = String::new();
    let mut lines = text.lines().peekable();
    while let Some(change) = lines.next() {
        changes.push_str(change);
        changes.push('\n');
        let fields: Vec<&str> = change.split('\t').collect();
        for (prefix, file, at) in [("  - ", &la, fields[1]), ("  + ", &lb, fields[2])] {
            if at == "-" {
                continue;
            }
            let record = lines.next().unwrap().strip_prefix(prefix).unwrap();
            let at: usize = at.parse().unwrap();
            assert_eq!(record.as_bytes(), file[at - 1], "{change}");
        }
    }
    assert_eq!(changes, String::from_utf8(plain.stdout).unwrap());
    assert!(changes.lines().count() > 50, "{changes}");
}
