//! A budget refusal names a `--mem` that gets past it, in every mode.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn rift(args: &[&str], mem: &str, a: &Path, b: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rift"))
        .args(args)
        .args(["--mem", mem])
        .arg(a)
        .arg(b)
        .output()
        .unwrap()
}

fn pair(dir: &Path, a: &str, b: &str) -> (PathBuf, PathBuf) {
    let (pa, pb) = (dir.join("a"), dir.join("b"));
    std::fs::write(&pa, a).unwrap();
    std::fs::write(&pb, b).unwrap();
    (pa, pb)
}

fn text(changed: bool) -> String {
    (0..2000)
        .map(|i| {
            if changed && i % 100 == 0 {
                format!("changed {i}\n")
            } else {
                format!("row {i}\n")
            }
        })
        .collect()
}

/// The `--mem` a refusal advises, if it is one.
fn advice(out: &Output) -> Option<String> {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let (_, mem) = stderr.split_once("; these files need --mem of at least ")?;
    Some(mem.trim_end().to_owned())
}

fn bytes(mem: &str) -> u64 {
    rift_core::Bytes::parse_budget(mem).unwrap().0
}

/// Follows the advice from `mem` until a run gets past the budget, and
/// returns the refusals on the way and the last run. Each refusal exits 2
/// and advises more than the limit it was given.
fn follow(args: &[&str], mut mem: String, a: &Path, b: &Path) -> (Vec<String>, Output) {
    let mut refused = Vec::new();
    for _ in 0..32 {
        let out = rift(args, &mem, a, b);
        let Some(next) = advice(&out) else {
            return (refused, out);
        };
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{stderr}");
        assert!(stderr.starts_with("rift: "), "{stderr}");
        assert!(stderr.contains(" budget exceeded: requested "), "{stderr}");
        assert!(bytes(&next) > bytes(&mem), "{mem} advised {next}");
        refused.push(mem);
        mem = next;
    }
    panic!("{args:?}: no --mem in 32 steps gets past the budget");
}

#[test]
fn summary_and_full_diff_advise_a_mem_that_works() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path(), &text(false), &text(true));
    for args in [&["--summary"][..], &[]] {
        let (refused, last) = follow(args, "16K".into(), &a, &b);
        assert!(!refused.is_empty(), "{args:?}: 16K is too small");
        assert_eq!(
            last.status.code(),
            Some(1),
            "{args:?}: {}",
            String::from_utf8_lossy(&last.stderr)
        );
    }
}

/// From just under what one worker needs, one step is enough.
#[test]
fn a_full_diff_short_of_one_worker_needs_one_rerun() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path(), &text(false), &text(true));
    let (refused, last) = follow(&[], "8M".into(), &a, &b);
    assert_eq!(refused, ["8M"]);
    assert_eq!(last.status.code(), Some(1));
}

/// The terminal view builds the diff as the unified diff does, and is
/// refused before it touches the terminal.
#[cfg(feature = "tui")]
#[test]
fn the_terminal_view_is_refused_like_the_diff() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path(), &text(false), &text(true));
    for mem in ["16K", "8M"] {
        let view = rift(&["-i"], mem, &a, &b);
        assert_eq!(view.status.code(), Some(2));
        let diff = rift(&[], mem, &a, &b);
        assert!(advice(&view).is_some(), "{mem}");
        assert_eq!(view.stderr, diff.stderr, "{mem}");
    }
}

#[test]
fn key_mode_advises_a_mem_that_works() {
    let dir = tempfile::tempdir().unwrap();
    let rows = |v: &str| -> String {
        std::iter::once("id,v\n".to_owned())
            .chain((0..2000).map(|i| format!("{i},{v}{i}\n")))
            .collect()
    };
    let (a, b) = pair(dir.path(), &rows("x"), &rows("y"));
    let (refused, last) = follow(&["--key", "csv:id"], "1K".into(), &a, &b);
    assert!(!refused.is_empty(), "1K is too small");
    assert_eq!(
        last.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&last.stderr)
    );
}
