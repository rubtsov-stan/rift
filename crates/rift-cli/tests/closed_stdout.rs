//! Standard output closed before Rift starts (`rift a b >&-`): Rust's
//! standard library drops writes to it without an error, so Rift would
//! exit as if it had printed the diff. It exits 2, as GNU diff does.

use std::process::Command;

#[test]
fn a_closed_standard_output_exits_two_with_a_message() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::write(&a, "x\n").unwrap();
    std::fs::write(&b, "y\n").unwrap();
    for flags in ["", "--summary", "--key csv:1"] {
        let out = Command::new("sh")
            .arg("-c")
            .arg(format!("\"$0\" {flags} \"$1\" \"$2\" >&-"))
            .args([env!("CARGO_BIN_EXE_rift")])
            .args([&a, &b])
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{flags:?}: {err}");
        assert_eq!(err, "rift: standard output is closed\n", "{flags:?}");
    }
}

#[test]
fn standard_output_sent_to_dev_null_is_not_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::write(&a, "x\n").unwrap();
    std::fs::write(&b, "y\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_rift"))
        .args([&a, &b])
        .stdout(std::process::Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(out.stderr, b"");
}
