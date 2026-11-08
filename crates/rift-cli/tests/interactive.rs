//! `-i` and `rift tui` without a terminal.
#![cfg(feature = "tui")]

use std::process::{Command, Stdio};

/// The check comes before the scan, so even a budget too small for the
/// files gets this error rather than a budget refusal.
#[test]
fn without_a_terminal_the_view_exits_two() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::write(&a, "x\n").unwrap();
    std::fs::write(&b, "y\n").unwrap();
    for head in [&["-i"][..], &["tui"], &["-i", "--mem", "64K"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_rift"))
            .args(head)
            .arg(&a)
            .arg(&b)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{head:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "rift: -i needs a terminal\n",
            "{head:?}"
        );
    }
}
