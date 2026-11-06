//! `-i` and `rift tui` in a build without the `tui` feature.
#![cfg(not(feature = "tui"))]

use std::process::Command;

#[test]
fn interactive_modes_exit_two() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::write(&a, "x\n").unwrap();
    std::fs::write(&b, "y\n").unwrap();
    for head in [&["-i"][..], &["tui"]] {
        let out = Command::new(env!("CARGO_BIN_EXE_rift"))
            .args(head)
            .arg(&a)
            .arg(&b)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{head:?}");
        assert_eq!(
            String::from_utf8_lossy(&out.stderr),
            "rift: built without TUI support\n"
        );
    }
}
