//! `rift --key` end to end, before key mode exists.

use std::process::Command;

fn rift(args: &[&str]) -> std::process::Output {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::write(&a, "id,v\n1,x\n").unwrap();
    std::fs::write(&b, "id,v\n1,y\n").unwrap();
    Command::new(env!("CARGO_BIN_EXE_rift"))
        .args(args)
        .arg(&a)
        .arg(&b)
        .output()
        .unwrap()
}

#[test]
fn a_valid_key_exits_two_as_not_implemented() {
    let out = rift(&["--key", "csv:id"]);
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert_eq!(stderr, "rift: key mode is not implemented yet\n");
    assert_eq!(out.stdout, b"");
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
