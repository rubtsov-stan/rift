//! `rift --summary` end to end: output and exit codes.

use std::path::Path;
use std::process::{Command, Output};

fn rift(a: &Path, b: &Path, mem: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rift"))
        .args(["--summary", "--mem", mem])
        .arg(a)
        .arg(b)
        .output()
        .unwrap()
}

fn files(a: &[u8], b: &[u8]) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let (pa, pb) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::write(&pa, a).unwrap();
    std::fs::write(&pb, b).unwrap();
    (dir, pa, pb)
}

fn text(n: usize) -> Vec<u8> {
    (0..n)
        .flat_map(|i| format!("row {i}\n").into_bytes())
        .collect()
}

#[test]
fn identical_files_exit_zero_with_one_equal_segment() {
    let data = text(10_000);
    let (_dir, a, b) = files(&data, &data);
    let out = rift(&a, &b, "16M");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{stdout}");
    assert!(stdout.contains("a: 10000 lines"), "{stdout}");
    assert!(stdout.contains("equal    1 segments"), "{stdout}");
    assert!(stdout.contains("differs  0 segments"), "{stdout}");
    assert!(stdout.contains("peak heap"), "{stdout}");
}

#[test]
fn differing_files_exit_one_and_list_the_range() {
    let a = text(10_000);
    let b = [&a[..], b"extra\n"].concat();
    let (_dir, pa, pb) = files(&a, &b);
    let out = rift(&pa, &pb, "16M");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    assert!(stdout.contains("differs  1 segments"), "{stdout}");
    assert!(stdout.contains("..10000  b "), "{stdout}");
}

#[test]
fn empty_files_are_identical() {
    let (_dir, a, b) = files(b"", b"");
    assert_eq!(rift(&a, &b, "16M").status.code(), Some(0));
    let (_dir, a, b) = files(b"", b"x\n");
    assert_eq!(rift(&a, &b, "16M").status.code(), Some(1));
}

#[test]
fn errors_exit_two() {
    let (dir, a, _) = files(b"x\n", b"x\n");
    let out = rift(&a, &dir.path().join("missing"), "16M");
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("missing"));
    let (_dir, a, b) = files(b"x\n", b"x\n");
    assert_eq!(rift(&a, &b, "12Q").status.code(), Some(2));
}

#[test]
fn mem_takes_the_same_sizes_as_the_generator() {
    let (_dir, a, b) = files(b"x\n", b"x\n");
    // Sizes a pair this small can run with; larger ones reserve sample
    // capacity in proportion to the budget, not the files.
    for mem in ["1GiB", "16mib", " 64M ", "65536k"] {
        assert_eq!(rift(&a, &b, mem).status.code(), Some(0), "--mem {mem:?}");
    }
    for mem in ["0", "0K", "1.5G", "-1M", "16Q"] {
        assert_eq!(rift(&a, &b, mem).status.code(), Some(2), "--mem {mem:?}");
    }
}

#[test]
fn ranges_past_the_first_ten_are_counted() {
    let a = text(2000);
    let mut b = a.clone();
    for at in (50..b.len()).step_by(500) {
        b[at] = b'#';
    }
    let (_dir, pa, pb) = files(&a, &b);
    let stdout = String::from_utf8(rift(&pa, &pb, "16M").stdout).unwrap();
    let differs: usize = stdout
        .lines()
        .find_map(|l| l.strip_prefix("differs  "))
        .and_then(|l| l.split(' ').next())
        .unwrap()
        .parse()
        .unwrap();
    assert!(differs > 10, "{stdout}");
    assert_eq!(stdout.lines().filter(|l| l.starts_with("  a ")).count(), 10);
    assert!(
        stdout.contains(&format!("  ... and {} more\n", differs - 10)),
        "{stdout}"
    );
    // Nothing is refined, so nothing is known to be approximate.
    assert!(!stdout.lines().any(|l| l.starts_with("approx")), "{stdout}");
}

#[test]
fn a_tiny_pair_under_a_huge_budget_reports_kibibytes_of_heap() {
    let (_dir, a, b) = files(b"x\nsame\n", b"y\nsame\n");
    let out = rift(&a, &b, "1T");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(1), "{stdout}");
    let peak = stdout.lines().find(|l| l.starts_with("peak heap")).unwrap();
    assert!(peak.contains(" KiB of 1.0 TiB"), "{peak}");
}
