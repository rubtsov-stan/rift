//! Standard output closed by its reader: `rift a b | head`, or git's pager
//! quitting. Plain mode exits 141, the status a shell reports for a `diff`
//! killed by SIGPIPE; `git-diff` exits 0 so git does not report a dead
//! external diff. Neither prints a message or panics (status 101).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_rift");

/// Large enough that the diff is well past the pipe buffer and Rift's own
/// 1 MiB output buffer.
const LINES: usize = 100_000;

fn numbered(suffix: &str) -> String {
    use std::fmt::Write;
    let mut s = String::new();
    for i in 0..LINES {
        writeln!(s, "{i}{suffix}").unwrap();
    }
    s
}

struct Pair {
    dir: tempfile::TempDir,
}

impl Pair {
    /// `a` and `b` of `LINES` lines, every line different.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a"), numbered("")).unwrap();
        fs::write(dir.path().join("b"), numbered("x")).unwrap();
        Self { dir }
    }

    fn a(&self) -> PathBuf {
        self.dir.path().join("a")
    }

    fn b(&self) -> PathBuf {
        self.dir.path().join("b")
    }
}

/// Runs `rift args` with standard output already closed at the reader, so
/// the first write fails whatever the timing.
fn run_closed(args: &[&Path]) -> Output {
    run_closed_with(&[], args)
}

fn run_closed_with(flags: &[&str], args: &[&Path]) -> Output {
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    Command::new(BIN)
        .args(flags)
        .args(args)
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .unwrap()
}

fn assert_141_and_silent(out: &Output) {
    assert_eq!(out.status.code(), Some(141), "{:?}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
}

#[test]
fn a_unified_diff_into_a_closed_pipe_exits_141_without_a_message() {
    let p = Pair::new();
    assert_141_and_silent(&run_closed(&[&p.a(), &p.b()]));
}

#[test]
fn summary_into_a_closed_pipe_exits_141_without_a_message() {
    let p = Pair::new();
    assert_141_and_silent(&run_closed_with(&["--summary"], &[&p.a(), &p.b()]));
}

#[test]
fn key_output_into_a_closed_pipe_exits_141_without_a_message() {
    let p = Pair::new();
    let out = run_closed_with(&["--key", "csv:1"], &[&p.a(), &p.b()]);
    assert_141_and_silent(&out);
}

#[test]
fn key_records_into_a_closed_pipe_exit_141_without_a_message() {
    let p = Pair::new();
    let out = run_closed_with(&["--key", "csv:1", "--show-records"], &[&p.a(), &p.b()]);
    assert_141_and_silent(&out);
}

#[test]
fn stats_do_not_hide_a_closed_pipe() {
    let p = Pair::new();
    let out = run_closed_with(&["--stats"], &[&p.a(), &p.b()]);
    assert_eq!(out.status.code(), Some(141), "{:?}", out.status);
}

#[test]
fn help_and_version_into_a_closed_pipe_do_not_panic() {
    for flag in ["--help", "--version"] {
        let out = run_closed_with(&[flag], &[]);
        assert_eq!(out.status.code(), Some(0), "{flag}: {:?}", out.status);
        assert_eq!(String::from_utf8_lossy(&out.stderr), "", "{flag}");
    }
}

#[test]
fn a_reader_that_quits_after_a_little_gives_the_shell_141() {
    let p = Pair::new();
    let out = Command::new("bash")
        .arg("-c")
        .arg(r#""$0" "$1" "$2" | head -c 10 >/dev/null; echo "${PIPESTATUS[0]}""#)
        .args([BIN])
        .args([p.a(), p.b()])
        .output();
    let Ok(out) = out else {
        eprintln!("skipped: bash is not on PATH");
        return;
    };
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "141");
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
}

fn git_diff_args(p: &Pair) -> Vec<String> {
    let hex = "a".repeat(40);
    let hex2 = "b".repeat(40);
    vec![
        "git-diff".into(),
        "f".into(),
        p.a().display().to_string(),
        hex,
        "100644".into(),
        p.b().display().to_string(),
        hex2,
        "100644".into(),
    ]
}

#[test]
fn git_diff_into_a_closed_pipe_exits_zero_without_a_message() {
    let p = Pair::new();
    let (reader, writer) = io::pipe().unwrap();
    drop(reader);
    let out = Command::new(BIN)
        .args(git_diff_args(&p))
        .stdout(writer)
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{:?}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
}
