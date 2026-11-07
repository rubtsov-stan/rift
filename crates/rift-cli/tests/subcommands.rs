//! `serve` and `git-diff` are subcommands; files of those names are
//! reached by a path, and `rift A B` parses as it always did.

use std::path::Path;
use std::process::{Command, Output};

fn rift(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rift"))
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap()
}

fn files(names: &[&str]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (i, name) in names.iter().enumerate() {
        std::fs::write(dir.path().join(name), format!("same\nfile {i}\n")).unwrap();
    }
    dir
}

#[test]
fn help_lists_both_subcommands() {
    let out = rift(Path::new("."), &["--help"]);
    assert!(out.status.success());
    let help = String::from_utf8(out.stdout).unwrap();
    assert!(help.contains("serve"), "{help}");
    assert!(help.contains("git-diff"), "{help}");
    let out = rift(Path::new("."), &["serve", "--help"]);
    assert!(String::from_utf8_lossy(&out.stdout).contains("--max-total-mem"));
}

#[test]
fn files_named_like_subcommands_are_reached_by_path_and_help_is_a_file() {
    for name in ["serve", "git-diff", "help"] {
        let dir = files(&[name, "other"]);
        let path = if name == "help" {
            name.to_owned()
        } else {
            format!("./{name}")
        };
        let out = rift(dir.path(), &[&path, "other"]);
        let diff = String::from_utf8_lossy(&out.stdout);
        assert_eq!(out.status.code(), Some(1), "{name}: {diff}");
        assert!(diff.contains("-file 0\n+file 1\n"), "{name}: {diff}");
    }
}

#[test]
fn a_file_named_tui_with_one_more_argument_is_still_a_file() {
    let dir = files(&["tui", "other"]);
    let out = rift(dir.path(), &["tui", "other"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("+file 1\n"));
}

#[test]
fn two_files_and_flags_parse_as_before() {
    let dir = files(&["a", "b"]);
    let out = rift(dir.path(), &["--mem", "16M", "--no-moves", "a", "b"]);
    assert_eq!(out.status.code(), Some(1));
    let out = rift(dir.path(), &["a", "b", "--summary"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stdout).contains("differs"));
    let out = rift(dir.path(), &["a"]);
    assert_eq!(out.status.code(), Some(2), "one file is a usage error");
}
