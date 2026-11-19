//! `rift A B` on two directories: the stream of `diff -r`-style lines and
//! per-file diffs, `--summary`, renames by content, the rules for links,
//! special files and unreadable entries, and output that depends only on
//! the trees and `--mem`.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_rift");

/// What a tree holds at a relative path.
#[derive(Clone, Copy)]
enum Node<'a> {
    File(&'a [u8]),
    Dir,
    Link(&'a str),
    Fifo,
}

use Node::{Dir, Fifo, File, Link};

/// Creates `nodes` under `root` in the order given (parents first), every
/// file with the same modification time so headers do not change between
/// runs.
fn build(root: &Path, nodes: &[(&str, Node<'_>)]) {
    fs::create_dir_all(root).unwrap();
    for (path, node) in nodes {
        let p = root.join(path);
        match node {
            File(bytes) => {
                fs::write(&p, bytes).unwrap();
                let f = fs::File::options().write(true).open(&p).unwrap();
                f.set_modified(UNIX_EPOCH + Duration::from_secs(1_700_000_000))
                    .unwrap();
            }
            Dir => fs::create_dir(&p).unwrap(),
            Link(target) => std::os::unix::fs::symlink(target, &p).unwrap(),
            Fifo => assert!(Command::new("mkfifo").arg(&p).status().unwrap().success()),
        }
    }
}

/// Runs `rift ARGS A B` in `dir` with names relative to it, in UTC.
fn rift_in(dir: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .current_dir(dir)
        .env("TZ", "UTC")
        .args(args)
        .output()
        .unwrap()
}

fn text(b: &[u8]) -> &str {
    std::str::from_utf8(b).unwrap()
}

const A_TREE: &[(&str, Node<'_>)] = &[
    ("sub", Dir),
    ("sub/changed.txt", File(b"one\ntwo\nthree\n")),
    ("same.txt", File(b"same\n")),
    ("old-name.txt", File(b"moved content\nline2\n")),
    ("removed.txt", File(b"gone\n")),
    ("onlydir", Dir),
    ("onlydir/f", File(b"x\n")),
    ("fd", Dir),
    ("fd/inner", File(b"y\n")),
    ("link", Link("same.txt")),
    ("sp ace", File(b"a\xffb\n")),
];

const B_TREE: &[(&str, Node<'_>)] = &[
    ("sub", Dir),
    ("sub/changed.txt", File(b"one\nTWO\nthree\n")),
    ("sub/new-name.txt", File(b"moved content\nline2\n")),
    ("same.txt", File(b"same\n")),
    ("added.txt", File(b"new\n")),
    ("empty", Dir),
    ("fd", File(b"file\n")),
    ("link", Link("other.txt")),
    ("sp ace", File(b"a\xffb\nz\n")),
];

const TS: &str = "2023-11-14 22:13:20.000000000 +0000";

fn example() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    build(&dir.path().join("A"), A_TREE);
    build(&dir.path().join("B"), B_TREE);
    dir
}

#[test]
fn two_directories_print_a_diff_r_stream() {
    let dir = example();
    let out = rift_in(dir.path(), &["A", "B"]);
    let want = format!(
        "Only in B: added.txt
Only in B: empty
File A/fd is a directory while file B/fd is a regular file
Symbolic links A/link and B/link differ
Only in A: onlydir
Only in A: removed.txt
diff -r \"A/sp ace\" \"B/sp ace\"
--- \"A/sp ace\"\t{TS}
+++ \"B/sp ace\"\t{TS}
@@ -1 +1,2 @@
 a\u{1}b
+z
diff -r A/sub/changed.txt B/sub/changed.txt
--- A/sub/changed.txt\t{TS}
+++ B/sub/changed.txt\t{TS}
@@ -1,3 +1,3 @@
 one
-two
+TWO
 three
Renamed: A/old-name.txt -> B/sub/new-name.txt
"
    )
    .replace('\u{1}', "\u{fffd}");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        want,
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stderr.is_empty(), "{}", text(&out.stderr));
}

#[test]
fn trailing_slashes_on_the_operands_change_nothing() {
    let dir = example();
    let plain = rift_in(dir.path(), &["A", "B"]);
    let slashed = rift_in(dir.path(), &["A/", "B//"]);
    assert_eq!(plain.stdout, slashed.stdout);
}

#[test]
fn the_summary_lists_one_status_per_path_then_counts() {
    let dir = example();
    let out = rift_in(dir.path(), &["--summary", "A", "B"]);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    let got: String = text(&out.stdout)
        .lines()
        .filter(|l| !l.starts_with("peak heap ") && !l.starts_with("time "))
        .flat_map(|l| [l, "\n"])
        .collect();
    assert_eq!(
        got,
        "A  added.txt
A  empty/
T  fd
M  link
D  onlydir/
D  removed.txt
M  \"sp ace\"
M  sub/changed.txt
R  old-name.txt -> sub/new-name.txt
added 1, removed 3, renamed 1, changed 3, type changed 1, same 1
"
    );
    assert!(text(&out.stdout).contains("\npeak heap "));
}

#[test]
fn equal_trees_print_nothing_and_exit_0() {
    let dir = tempfile::tempdir().unwrap();
    build(&dir.path().join("A"), A_TREE);
    build(&dir.path().join("B"), A_TREE);
    let out = rift_in(dir.path(), &["A", "B"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
}

#[test]
fn a_directory_and_a_file_are_refused() {
    let dir = example();
    for args in [&["A", "B/added.txt"][..], &["B/added.txt", "A"]] {
        let out = rift_in(dir.path(), args);
        assert_eq!(out.status.code(), Some(2));
        assert!(
            text(&out.stderr).contains("two files or two directories"),
            "{}",
            text(&out.stderr)
        );
    }
    for opt in [&["--key", "csv:1"][..], &["-i"]] {
        let mut args = opt.to_vec();
        args.extend(["A", "B"]);
        let out = rift_in(dir.path(), &args);
        assert_eq!(out.status.code(), Some(2), "{opt:?}");
        assert!(
            text(&out.stderr).contains("not directories"),
            "{opt:?}: {}",
            text(&out.stderr)
        );
    }
}

#[test]
fn files_with_a_nul_in_the_first_8k_are_binary() {
    let dir = tempfile::tempdir().unwrap();
    let mut late = vec![b'x'; 9000];
    late.push(0);
    build(
        &dir.path().join("A"),
        &[("bin", File(b"a\0b\n")), ("late", File(&late))],
    );
    let mut late_b = late.clone();
    late_b.push(b'\n');
    build(
        &dir.path().join("B"),
        &[("bin", File(b"a\0c\n")), ("late", File(&late_b))],
    );
    let out = rift_in(dir.path(), &["A", "B"]);
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("Binary files A/bin and B/bin differ\ndiff -r A/late B/late\n"),
        "{stdout}"
    );
}

/// Several files with one content: removed and added files pair in byte
/// order of their paths, first with first, whatever order the file system
/// lists them in; the rest stay only in their tree.
#[test]
fn duplicates_pair_in_path_order() {
    let a: &[(&str, Node<'_>)] = &[
        ("d", Dir),
        ("d/x2", File(b"dup\n")),
        ("d/x1", File(b"dup\n")),
        ("keep", File(b"k\n")),
    ];
    let b: &[(&str, Node<'_>)] = &[
        ("e", Dir),
        ("e/y3", File(b"dup\n")),
        ("e/y1", File(b"dup\n")),
        ("e/y2", File(b"dup\n")),
        ("keep", File(b"k\n")),
    ];
    let want = "Only in A: d
Only in B: e
Renamed: A/d/x1 -> B/e/y1
Renamed: A/d/x2 -> B/e/y2
";
    for order in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let (mut ra, mut rb) = (a.to_vec(), b.to_vec());
        if order {
            // Parents first, files in the other order.
            ra[1..3].reverse();
            rb[1..4].reverse();
        }
        build(&dir.path().join("A"), &ra);
        build(&dir.path().join("B"), &rb);
        let out = rift_in(dir.path(), &["A", "B"]);
        assert_eq!(text(&out.stdout), want, "{}", text(&out.stderr));
    }
}

#[test]
fn links_are_compared_by_target_and_never_followed() {
    let dir = tempfile::tempdir().unwrap();
    let nodes = [("loop", Link(".")), ("dangling", Link("nowhere"))];
    build(&dir.path().join("A"), &nodes);
    build(&dir.path().join("B"), &nodes);
    let out = rift_in(dir.path(), &["A", "B"]);
    assert_eq!(out.status.code(), Some(0), "{}", text(&out.stderr));
    assert!(out.stdout.is_empty(), "{}", text(&out.stdout));
}

#[test]
fn special_files_and_unreadable_entries_are_trouble_but_the_rest_is_printed() {
    let dir = tempfile::tempdir().unwrap();
    build(
        &dir.path().join("A"),
        &[
            ("p", Fifo),
            ("locked", Dir),
            ("locked/f", File(b"1\n")),
            ("z", File(b"a\n")),
        ],
    );
    build(
        &dir.path().join("B"),
        &[
            ("p", Fifo),
            ("locked", Dir),
            ("locked/f", File(b"2\n")),
            ("z", File(b"b\n")),
        ],
    );
    let locked = dir.path().join("A/locked");
    fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    // Root reads it anyway.
    let root = fs::read_dir(&locked).is_ok();
    let out = rift_in(dir.path(), &["A", "B"]);
    fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("rift: A/p: not compared: fifo"), "{stderr}");
    if !root {
        assert!(
            stderr.contains("A/locked") && stderr.contains("ermission denied"),
            "{stderr}"
        );
    }
    assert!(
        text(&out.stdout).contains("diff -r A/z B/z\n"),
        "{}",
        text(&out.stdout)
    );
}

#[test]
fn an_unreadable_file_is_named_on_the_side_that_failed() {
    let dir = tempfile::tempdir().unwrap();
    build(&dir.path().join("A"), &[("x", File(b"1\n"))]);
    build(&dir.path().join("B"), &[("x", File(b"2\n"))]);
    let locked = dir.path().join("B/x");
    fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o000)).unwrap();
    // Root reads it anyway.
    let root = fs::File::open(&locked).is_ok();
    let out = rift_in(dir.path(), &["A", "B"]);
    fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    if !root {
        let stderr = text(&out.stderr);
        assert!(stderr.contains("rift: B/x: "), "{stderr}");
        assert!(!stderr.contains("A/x"), "{stderr}");
    }
}

#[test]
fn a_link_that_cannot_be_read_is_named_on_the_side_that_failed() {
    let dir = tempfile::tempdir().unwrap();
    build(&dir.path().join("A"), &[("d", Dir), ("d/l", Link("t1"))]);
    build(&dir.path().join("B"), &[("d", Dir), ("d/l", Link("t2"))]);
    // Readable but not searchable: the entry is listed, its target is not.
    let locked = dir.path().join("B/d");
    fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
    let root = fs::read_link(locked.join("l")).is_ok();
    let out = rift_in(dir.path(), &["A", "B"]);
    fs::set_permissions(&locked, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    if !root {
        let stderr = text(&out.stderr);
        assert!(stderr.contains("rift: B/d/l: "), "{stderr}");
        assert!(!stderr.contains("A/d/l"), "{stderr}");
    }
}

/// Many renamed files at a small `--mem`: the rename table fills, the
/// first line says so, and every file is still accounted for, renamed or
/// only in one tree.
#[test]
fn a_full_rename_table_is_marked_approximate() {
    let dir = tempfile::tempdir().unwrap();
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for d in 0..40 {
        a.push((format!("old{d:02}"), None));
        b.push((format!("new{d:02}"), None));
        for f in 0..100 {
            let body = format!("file {d:02} {f:03} with a body of fixed length\n");
            a.push((
                format!("old{d:02}/a-long-name-for-a-file-{f:03}"),
                Some(body.clone()),
            ));
            b.push((
                format!("new{d:02}/b-long-name-for-a-file-{f:03}"),
                Some(body),
            ));
        }
    }
    let nodes = |v: &'_ [(String, Option<String>)]| -> Vec<(String, Option<Vec<u8>>)> {
        v.iter()
            .map(|(p, b)| (p.clone(), b.clone().map(String::into_bytes)))
            .collect()
    };
    for (root, list) in [("A", nodes(&a)), ("B", nodes(&b))] {
        let owned: Vec<(&str, Node<'_>)> = list
            .iter()
            .map(|(p, b)| (p.as_str(), b.as_deref().map_or(Dir, File)))
            .collect();
        build(&dir.path().join(root), &owned);
    }
    let out = rift_in(dir.path(), &["--mem", "2M", "A", "B"]);
    let stdout = text(&out.stdout);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
    assert!(
        stdout.starts_with("Renames approximate: the rename table is full at --mem 2048K;"),
        "{}",
        &stdout[..stdout.len().min(300)]
    );
    let renamed = stdout
        .lines()
        .filter(|l| l.starts_with("Renamed: "))
        .count();
    assert!(renamed > 0 && renamed < 4000, "{renamed}");
    let full = rift_in(dir.path(), &["--mem", "64M", "A", "B"]);
    let all = text(&full.stdout);
    assert!(!all.contains("approximate"));
    assert_eq!(
        all.lines().filter(|l| l.starts_with("Renamed: ")).count(),
        4000
    );
}

/// A directory whose listing does not fit is refused with a `--mem` to
/// rerun with, and that `--mem` works.
#[test]
fn an_oversized_listing_is_refused_with_advice_that_works() {
    let dir = tempfile::tempdir().unwrap();
    for root in ["A", "B"] {
        let r = dir.path().join(root);
        fs::create_dir(&r).unwrap();
        for i in 0..20_000 {
            fs::write(r.join(format!("entry-{i:06}")), "").unwrap();
        }
    }
    let out = rift_in(dir.path(), &["--mem", "1M", "A", "B"]);
    let stderr = text(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("budget exceeded"), "{stderr}");
    let advised = stderr
        .split_once("rerun with --mem ")
        .unwrap_or_else(|| panic!("{stderr}"))
        .1
        .trim();
    let ok = rift_in(dir.path(), &["--mem", advised, "A", "B"]);
    assert_eq!(ok.status.code(), Some(0), "{advised}: {}", text(&ok.stderr));
}

/// The same trees and `--mem` give the same bytes whatever order the
/// files were created in and however many threads run.
#[test]
fn output_depends_only_on_the_trees_and_mem() {
    let mut config = rift_gen::Config::new(7, 1 << 20, rift_gen::Profile::Sql);
    config.change_ratio = 0.01;
    let (mut big_a, mut big_b, mut m) = (Vec::new(), Vec::new(), Vec::new());
    rift_gen::generate(&config, &mut big_a, &mut big_b, &mut m).unwrap();
    let mut a: Vec<(String, Vec<u8>)> = Vec::new();
    let mut b: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..30 {
        a.push((format!("f{i:02}"), format!("{i}\nshared\n").into_bytes()));
        b.push((
            format!("f{i:02}"),
            format!("{}\nshared\n", i % 7).into_bytes(),
        ));
        a.push((
            format!("r{i:02}"),
            format!("renamed {}\n", i % 5).into_bytes(),
        ));
        b.push((
            format!("s{i:02}"),
            format!("renamed {}\n", i % 5).into_bytes(),
        ));
    }
    a.push(("big.sql".into(), big_a));
    b.push(("big.sql".into(), big_b));
    let mut outputs = Vec::new();
    for reverse in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        for (root, list) in [("A", &a), ("B", &b)] {
            let mut nodes: Vec<(&str, Node<'_>)> =
                list.iter().map(|(p, c)| (p.as_str(), File(c))).collect();
            if reverse {
                nodes.reverse();
            }
            build(&dir.path().join(root), &nodes);
        }
        for (var, n) in [
            ("RAYON_NUM_THREADS", "1"),
            ("RAYON_NUM_THREADS", "8"),
            ("RIFT_CORES", "1"),
        ] {
            for mode in [&["--mem", "16M"][..], &["--mem", "16M", "--summary"]] {
                let mut args = mode.to_vec();
                args.extend(["A", "B"]);
                let out = Command::new(BIN)
                    .current_dir(dir.path())
                    .env("TZ", "UTC")
                    .env_remove("RAYON_NUM_THREADS")
                    .env_remove("RIFT_CORES")
                    .env(var, n)
                    .args(&args)
                    .output()
                    .unwrap();
                assert_eq!(out.status.code(), Some(1), "{}", text(&out.stderr));
                let kept: String = text(&out.stdout)
                    .lines()
                    .filter(|l| !l.starts_with("peak heap ") && !l.starts_with("time "))
                    .flat_map(|l| [l, "\n"])
                    .collect();
                outputs.push((format!("{reverse} {var}={n} {mode:?}"), kept));
            }
        }
    }
    for (name, out) in &outputs[2..] {
        let first = if name.contains("--summary") {
            &outputs[1]
        } else {
            &outputs[0]
        };
        assert!(out == &first.1, "{name} differs from {}", first.0);
    }
    assert!(outputs[0].1.contains("diff -r A/big.sql B/big.sql\n"));
}

#[cfg(target_os = "linux")]
#[test]
fn names_that_are_not_utf8_are_quoted_like_gnu_diff() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir(dir.path().join("A")).unwrap();
    fs::create_dir(dir.path().join("B")).unwrap();
    fs::write(
        dir.path()
            .join("A")
            .join(std::ffi::OsStr::from_bytes(b"caf\xe9")),
        "x\n",
    )
    .unwrap();
    let out = rift_in(dir.path(), &["A", "B"]);
    assert_eq!(text(&out.stdout), "Only in A: \"caf\\351\"\n");
}
