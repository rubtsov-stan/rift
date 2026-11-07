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
    let (_, mem) = stderr.split_once("; rerun with --mem ")?;
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

/// A record larger than the budget allows is a budget refusal too. The
/// read windows grow with --mem, so following the advice from far below
/// takes many small steps; from just below the smallest --mem that works,
/// one step is enough.
#[test]
fn key_mode_advises_a_mem_for_a_long_record() {
    let dir = tempfile::tempdir().unwrap();
    let long = "x".repeat(200_000);
    let (a, b) = pair(
        dir.path(),
        &format!("id,v\n1,{long}\n2,y\n"),
        &format!("id,v\n2,y\n1,{long}z\n"),
    );
    let args = ["--key", "csv:id"];
    let kib = |k: u64| format!("{k}K");
    // The smallest limit in KiB, by bisection; every refusal on the way
    // advises more than it was given.
    let (mut lo, mut hi) = (64, 8 << 10);
    assert!(advice(&rift(&args, &kib(lo), &a, &b)).is_some());
    assert_eq!(rift(&args, &kib(hi), &a, &b).status.code(), Some(1));
    while hi - lo > 1 {
        let mid = u64::midpoint(lo, hi);
        let out = rift(&args, &kib(mid), &a, &b);
        if let Some(next) = advice(&out) {
            assert_eq!(out.status.code(), Some(2));
            assert!(bytes(&next) > mid << 10, "{mid}K advised {next}");
            lo = mid;
        } else {
            assert_eq!(out.status.code(), Some(1), "{mid}K");
            hi = mid;
        }
    }
    let (refused, last) = follow(&args, kib(lo), &a, &b);
    assert_eq!(refused, [kib(lo)]);
    assert_eq!(String::from_utf8_lossy(&last.stdout), "~1\t2\t3\n");
}
