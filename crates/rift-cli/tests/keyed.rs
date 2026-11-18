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
    // The records' buffer comes out of the budget, so the keys can split
    // into other passes, and each pass prints in its own order.
    let sorted = |text: &str| {
        let mut lines: Vec<&str> = text.lines().collect();
        lines.sort_unstable();
        lines.join("\n")
    };
    let plain = String::from_utf8(plain.stdout).unwrap();
    assert_eq!(sorted(&changes), sorted(&plain));
    assert!(changes.lines().count() > 50, "{changes}");
}

fn stdout_of(out: &Output, code: i32) -> String {
    assert_eq!(
        out.status.code(),
        Some(code),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone()).unwrap()
}

#[test]
fn show_fields_names_the_fields_of_a_changed_key() {
    let out = rift_on(
        b"id,name,price,note\n1,apple,10,\"red, sweet\"\n3,plum,5,\"two\nlines\"\n4,fig,3,x\n",
        b"id,name,price,note\n3,plum,5,\"two\nLINES\"\n1,apple,12,\"red, sour\"\n5,kiwi,4,new\n",
        &["--key", "csv:id", "--show-fields"],
    );
    assert_eq!(
        stdout_of(&out, 1),
        "~1\t2\t4\n    price: 10 -> 12\n    note: \"red, sweet\" -> \"red, sour\"\n\
         ~3\t3\t2\n    note: \"two\\nlines\" -> \"two\\nLINES\"\n\
         -4\t5\t-\n+5\t-\t5\n"
    );
}

#[test]
fn show_fields_marks_missing_fields_and_layout_only_changes() {
    let out = rift_on(
        b"{\"id\":1,\"old\":true}\n{\"id\":2,\"v\":1}\n{\"id\":3,\"v\":tru e}\n",
        b"{\"id\":1,\"new\":[1, 2]}\n{\"v\":1,\"id\":2}\n{\"id\":3,\"v\":true}\n",
        &["--key", "json:id", "--show-fields"],
    );
    assert_eq!(
        stdout_of(&out, 1),
        "~1\t1\t1\n    old: true -> (none)\n    new: (none) -> [1, 2]\n\
         ~2\t2\t2\n    (no field differs)\n\
         ~3\t3\t3\n    (record not split into fields)\n"
    );
}

#[test]
fn long_values_are_cut_on_a_character_boundary() {
    // 63 ASCII bytes, then a two-byte character across the 64-byte cut.
    let long = format!("{}é tail", "a".repeat(63));
    let a = format!("id,v\n1,{long}\n2,x\ty\n");
    let b = format!("id,v\n1,{long}!\n2,x\tz\n");
    let out = rift_on(
        a.as_bytes(),
        b.as_bytes(),
        &["--key", "csv:id", "--show-fields"],
    );
    let cut = format!("{}...", "a".repeat(63));
    assert_eq!(
        stdout_of(&out, 1),
        format!("~1\t2\t2\n    v: {cut} -> {cut}\n~2\t3\t3\n    v: x\\ty -> x\\tz\n")
    );
}

#[test]
fn fields_limit_the_comparison_and_the_exit_status() {
    let (a, b) = (b"id,price,note\n1,10,a\n", b"id,price,note\n1,10,b\n");
    let out = rift_on(a, b, &["--key", "csv:id", "--fields", "price"]);
    assert_eq!(stdout_of(&out, 0), "");
    let out = rift_on(a, b, &["--key", "csv:id", "--fields", "price,note"]);
    assert_eq!(stdout_of(&out, 1), "~1\t2\t2\n    note: a -> b\n");
}

#[test]
fn a_field_in_no_header_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let (pa, pb) = (dir.path().join("old.csv"), dir.path().join("new.csv"));
    std::fs::write(&pa, b"id,v\n1,x\n").unwrap();
    std::fs::write(&pb, b"id,v\n1,y\n").unwrap();
    let out = rift_paths(&pa, &pb, &["--key", "csv:id", "--fields", "w"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        format!("rift: a ({}) has no column \"w\"\n", pa.display())
    );
}

#[test]
fn show_fields_come_before_the_records() {
    let out = rift_on(
        b"id,v\n1,\"x\ny\"\n",
        b"id,v\n1,z\n",
        &["--key", "csv:id", "--show-fields", "--show-records"],
    );
    assert_eq!(
        stdout_of(&out, 1),
        "~1\t2\t2\n    v: \"x\\ny\" -> z\n  - 1,\"x\n  - y\"\n  + 1,z\n"
    );
}

#[test]
fn show_fields_needs_key() {
    for flag in [&["--show-fields"][..], &["--fields", "a"]] {
        let out = rift(flag);
        assert_eq!(out.status.code(), Some(2));
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert!(stderr.contains("--key <SPEC>"), "{stderr}");
    }
}

/// A record pair that does not fit is a budget error naming a --mem, and
/// that --mem prints the fields.
#[test]
fn a_record_pair_too_large_for_the_budget_names_a_mem_that_fits() {
    let long = "x".repeat(400 << 10);
    let a = format!("id,v\n1,{long}\n");
    let b = format!("id,v\n1,{long}y\n");
    let flags = |mem: &str| {
        vec![
            "--key".to_owned(),
            "csv:id".into(),
            "--show-fields".into(),
            "--mem".into(),
            mem.into(),
        ]
    };
    let mut mem = "1M".to_owned();
    for _ in 0..4 {
        let owned = flags(&mem);
        let borrowed: Vec<&str> = owned.iter().map(String::as_str).collect();
        let out = rift_on(a.as_bytes(), b.as_bytes(), &borrowed);
        if out.status.code() == Some(1) {
            let text = String::from_utf8(out.stdout).unwrap();
            assert!(text.starts_with("~1\t2\t2\n    v: xxx"), "{text}");
            assert!(mem != "1M", "1M was expected to be refused");
            return;
        }
        let stderr = String::from_utf8(out.stderr).unwrap();
        assert_eq!(out.status.code(), Some(2), "{stderr}");
        mem = stderr
            .rsplit_once("rerun with --mem ")
            .unwrap_or_else(|| panic!("{stderr}"))
            .1
            .trim()
            .to_owned();
    }
    panic!("no --mem fitted after four reruns");
}

#[test]
fn copy_key_lines_name_their_table_in_every_mode() {
    let a = b"COPY public.item (id, v) FROM stdin;\n1\tx\n\\.\n\
              COPY public.order (id, v) FROM stdin;\n1\tx\n2\tx\n\\.\n";
    let b = b"COPY public.item (id, v) FROM stdin;\n1\ty\n\\.\n\
              COPY public.order (id, v) FROM stdin;\n1\ty\n\\.\n";
    let plain = "~public.item:1\t2\t2\n~public.order:1\t5\t5\n-public.order:2\t6\t-\n";
    let out = rift_on(a, b, &["--key", "copy:id"]);
    assert_eq!(stdout_of(&out, 1), plain);
    let out = rift_on(a, b, &["--key", "copy:id", "--show-records"]);
    assert_eq!(
        stdout_of(&out, 1),
        "~public.item:1\t2\t2\n  - 1\tx\n  + 1\ty\n\
         ~public.order:1\t5\t5\n  - 1\tx\n  + 1\ty\n\
         -public.order:2\t6\t-\n  - 2\tx\n"
    );
    let out = rift_on(a, b, &["--key", "copy:id", "--fields", "v"]);
    assert_eq!(
        stdout_of(&out, 1),
        "~public.item:1\t2\t2\n    v: x -> y\n\
         ~public.order:1\t5\t5\n    v: x -> y\n\
         -public.order:2\t6\t-\n"
    );
}

#[test]
fn duplicate_copy_keys_name_their_table() {
    let a = b"COPY public.item (id) FROM stdin;\n7\n7\n\\.\n";
    let out = rift_on(a, a, &["--key", "copy:id"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "rift: duplicate keys: public.item:7\n"
    );
}
