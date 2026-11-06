//! `rift a b` end to end: the output is a patch that turns A into B, and
//! the exit code follows `diff`.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rift_gen::{Config, Kind, Profile};

fn rift(args: &[&str], a: &Path, b: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rift"))
        .args(args)
        .arg(a)
        .arg(b)
        .output()
        .unwrap()
}

struct Pair {
    _dir: tempfile::TempDir,
    a: PathBuf,
    b: PathBuf,
}

fn pair(a: &[u8], b: &[u8]) -> Pair {
    let dir = tempfile::tempdir().unwrap();
    let (pa, pb) = (dir.path().join("a"), dir.path().join("b"));
    fs::write(&pa, a).unwrap();
    fs::write(&pb, b).unwrap();
    Pair {
        _dir: dir,
        a: pa,
        b: pb,
    }
}

/// `patch` binaries on this machine: the system one and GNU's `gpatch`.
fn patchers() -> Vec<&'static str> {
    ["patch", "gpatch"]
        .into_iter()
        .filter(|p| Command::new(p).arg("--version").output().is_ok())
        .collect()
}

/// Applies `diff` to a copy of `a` with every patcher found and checks the
/// result is `b`.
fn patch_gives_b(a: &Path, b: &Path, diff: &[u8]) {
    let want = fs::read(b).unwrap();
    if diff.is_empty() {
        // Nothing to apply; the system patch would not even write the
        // output file.
        assert!(
            fs::read(a).unwrap() == want,
            "empty diff of different files"
        );
        return;
    }
    let found = patchers();
    assert!(!found.is_empty(), "no patch binary");
    for patcher in found {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        fs::copy(a, &target).unwrap();
        fs::write(dir.path().join("d.patch"), diff).unwrap();
        let out = Command::new(patcher)
            .args(["-s", "-o", "out", "target", "d.patch"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "{patcher}: {out:?}");
        let got = fs::read(dir.path().join("out")).unwrap_or_default();
        assert!(got == want, "{patcher} did not produce B");
    }
}

fn generated(seed: u64, size: u64, profile: Profile, kinds: Option<&[Kind]>) -> Pair {
    let mut config = Config::new(seed, size, profile);
    config.change_ratio = 0.003;
    if let Some(k) = kinds {
        config.kinds = k.to_vec();
    }
    let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
    rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
    pair(&a, &b)
}

#[test]
fn generated_pairs_patch_from_a_to_b() {
    let cases = [
        (1, 4 << 20, Profile::Sql, None),
        (2, 4 << 20, Profile::Csv, None),
        (3, 2 << 20, Profile::Ndjson, None),
        (4, 2 << 20, Profile::Text, None),
        (
            5,
            80 << 20,
            Profile::Text,
            Some(&[Kind::Long, Kind::Modify][..]),
        ),
        (
            6,
            2 << 20,
            Profile::Sql,
            Some(&[Kind::Crlf, Kind::NoEol, Kind::Modify][..]),
        ),
        (
            7,
            4 << 20,
            Profile::Sql,
            Some(&[Kind::Dup, Kind::LowUniq, Kind::Move][..]),
        ),
    ];
    for (seed, size, profile, kinds) in cases {
        let p = generated(seed, size, profile, kinds);
        for mem in ["16M", "256M"] {
            let out = rift(&["--mem", mem], &p.a, &p.b);
            assert_eq!(
                out.status.code(),
                Some(1),
                "seed {seed} mem {mem}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            patch_gives_b(&p.a, &p.b, &out.stdout);
        }
    }
}

/// A region with no unique line, too large for one job at 16 MiB: it
/// cannot be resampled and comes out as one approximate replacement, which
/// still patches.
#[test]
fn approximate_regions_are_marked_and_still_patch() {
    let a = b"same line\n".repeat(300_000);
    let mut b = a.clone();
    b[10 * 150_000] = b'S';
    let p = pair(&a, &b);
    let out = rift(&["--mem", "16M", "--stats"], &p.a, &p.b);
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    let header = text.lines().find(|l| l.starts_with("@@")).unwrap();
    assert!(header.ends_with("@@ approx"), "{header}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("approx 1 segments"), "{stderr}");
    patch_gives_b(&p.a, &p.b, &out.stdout);

    // With room to refine it, the same pair diffs exactly.
    let out = rift(&["--mem", "256M"], &p.a, &p.b);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(!text.contains("approx"), "{text}");
    assert_eq!(text.lines().filter(|l| l.starts_with('-')).count(), 2);
    patch_gives_b(&p.a, &p.b, &out.stdout);
}

#[test]
fn exit_codes_follow_diff() {
    let same = pair(b"one\ntwo\n", b"one\ntwo\n");
    let out = rift(&[], &same.a, &same.b);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(out.stdout, b"");

    let differ = pair(b"one\ntwo\n", b"one\n2\n");
    let out = rift(&[], &differ.a, &differ.b);
    assert_eq!(out.status.code(), Some(1));
    patch_gives_b(&differ.a, &differ.b, &out.stdout);

    let missing = differ.a.with_file_name("missing");
    let out = rift(&[], &differ.a, &missing);
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("missing"));

    let out = rift(&["--mem", "64K"], &differ.a, &differ.b);
    assert_eq!(out.status.code(), Some(2), "--mem is honoured by the diff");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.starts_with("rift: skeleton budget exceeded"),
        "{stderr}"
    );
}

#[test]
fn stats_report_peak_heap_on_stderr_only() {
    let p = generated(9, 2 << 20, Profile::Sql, None);
    let out = rift(&["--stats", "--mem", "64M"], &p.a, &p.b);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("peak heap"), "{stderr}");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("peak heap"));
    patch_gives_b(&p.a, &p.b, &out.stdout);
}

#[test]
fn pathological_inputs() {
    let cases: [(&[u8], &[u8]); 7] = [
        (b"", b""),
        (b"", b"new\nfile\n"),
        (b"old\nfile\n", b""),
        (b"\n\n\n", b"\n\n"),
        (b"no newline", b"no newline\n"),
        (b"x\nx\nx\nx\n", b"x\nx\ny\nx\nx\n"),
        (b"a\r\nb\r\n", b"a\nb\n"),
    ];
    for (a, b) in cases {
        let p = pair(a, b);
        let out = rift(&[], &p.a, &p.b);
        assert_eq!(out.status.code(), Some(i32::from(a != b)), "{a:?} {b:?}");
        if a != b {
            patch_gives_b(&p.a, &p.b, &out.stdout);
        }
    }
}

/// One line of `len` bytes, the last byte before the newline set to `end`.
fn one_long_line(path: &Path, len: usize, end: u8) {
    use std::io::Write;
    let mut f = std::io::BufWriter::new(fs::File::create(path).unwrap());
    let block = vec![b'z'; 1 << 20];
    let mut left = len - 2;
    while left > 0 {
        let n = left.min(block.len());
        f.write_all(&block[..n]).unwrap();
        left -= n;
    }
    f.write_all(&[end, b'\n']).unwrap();
}

#[test]
#[ignore = "writes two 1 GiB files; run in release"]
fn a_single_1gib_line() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    one_long_line(&a, 1 << 30, b'a');
    one_long_line(&b, 1 << 30, b'a');
    let out = rift(&["--stats"], &a, &b);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    one_long_line(&b, 1 << 30, b'b');
    let patch = dir.path().join("d.patch");
    let status = Command::new(env!("CARGO_BIN_EXE_rift"))
        .arg("--stats")
        .arg(&a)
        .arg(&b)
        .stdout(fs::File::create(&patch).unwrap())
        .stderr(fs::File::create(dir.path().join("stats")).unwrap())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1));
    let report = fs::read_to_string(dir.path().join("stats")).unwrap();
    eprintln!("{report}");
    let peak = report.lines().find(|l| l.starts_with("peak heap")).unwrap();
    let bytes: u64 = peak
        .rsplit('(')
        .next()
        .unwrap()
        .split(' ')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(bytes <= 256 << 20, "{peak}");
    assert!(fs::metadata(&patch).unwrap().len() > 2 << 30);
    // GNU patch takes lines of any length; the system patch on macOS
    // may not, so only gpatch is asked when it is there.
    let gnu = if patchers().contains(&"gpatch") {
        "gpatch"
    } else {
        "patch"
    };
    let out = Command::new(gnu)
        .args(["-s", "-o", "out"])
        .arg(&a)
        .arg(&patch)
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let same = Command::new("cmp")
        .arg(dir.path().join("out"))
        .arg(&b)
        .status()
        .unwrap();
    assert!(same.success(), "patched A differs from B");
}

/// Lines a unified diff adds or removes, headers excluded.
fn changed_lines(diff: &[u8]) -> u64 {
    diff.split(|&c| c == b'\n')
        .filter(|l| {
            (l.starts_with(b"+") && !l.starts_with(b"+++"))
                || (l.starts_with(b"-") && !l.starts_with(b"---"))
        })
        .count() as u64
}

/// Against `git diff --no-index --patience` on a corpus of small generated
/// pairs: every rift diff patches A into B, and rift changes at most 10%
/// more lines than git in total. Patience diff is not minimal either, and
/// rift's anchors are a sample, so the scripts differ in detail.
#[test]
fn differential_against_git_patience() {
    let (mut ours, mut git) = (0u64, 0u64);
    let profiles = [Profile::Sql, Profile::Csv, Profile::Ndjson, Profile::Text];
    for seed in 0..40u64 {
        let profile = profiles[usize::try_from(seed).unwrap() % profiles.len()];
        let p = generated(100 + seed, 64 << 10, profile, None);
        let out = rift(&[], &p.a, &p.b);
        assert!(out.status.code().is_some_and(|c| c <= 1), "{out:?}");
        patch_gives_b(&p.a, &p.b, &out.stdout);
        ours += changed_lines(&out.stdout);
        let reference = Command::new("git")
            .args([
                "diff",
                "--no-index",
                "--patience",
                "--no-color",
                "--no-ext-diff",
            ])
            .arg(&p.a)
            .arg(&p.b)
            .output()
            .unwrap();
        assert!(reference.status.code().is_some_and(|c| c <= 1));
        git += changed_lines(&reference.stdout);
    }
    eprintln!("changed lines: rift {ours}, git --patience {git}");
    assert!(ours * 10 <= git * 11, "rift {ours} against git {git}");
}
