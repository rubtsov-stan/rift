use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};

use imara_diff::{Algorithm, Diff, InternedInput};
use proptest::prelude::*;

use super::*;
use crate::lines::MemLines;
use crate::script::testgen::cases;

fn label(name: &'static str) -> Label<'static> {
    Label {
        name: name.as_bytes(),
        mtime: None,
    }
}

fn hunk(edits: Vec<Edit>) -> Hunk {
    Hunk {
        edits,
        approx: false,
    }
}

fn options(context: u64) -> Options<'static> {
    Options {
        context,
        old: label("a"),
        new: label("b"),
    }
}

fn render(a: &[u8], b: &[u8], hunks: &[Hunk], opts: &Options<'_>) -> (Vec<u8>, Verdict) {
    let mut out = Vec::new();
    let get = |i: usize| Ok(hunks.get(i));
    let verdict = write_diff(&MemLines::new(a), &MemLines::new(b), get, opts, &mut out).unwrap();
    (out, verdict)
}

/// A minimal script from imara-diff's Myers, as one hunk.
fn script(a: &[u8], b: &[u8]) -> Vec<Hunk> {
    let input = InternedInput::new(a, b);
    let diff = Diff::compute(Algorithm::Myers, &input);
    let mut edits = Vec::new();
    let (mut la, mut lb) = (0u64, 0u64);
    let equal = |la: &mut u64, lb: &mut u64, to: u64, edits: &mut Vec<Edit>| {
        if to > *la {
            edits.push(Edit::Equal {
                a: LineNo(*la),
                b: LineNo(*lb),
                len: to - *la,
            });
            *lb += to - *la;
            *la = to;
        }
    };
    for h in diff.hunks() {
        equal(&mut la, &mut lb, u64::from(h.before.start), &mut edits);
        if !h.before.is_empty() {
            edits.push(Edit::Delete {
                a: LineNo(la),
                len: u64::from(h.before.end - h.before.start),
            });
            la = u64::from(h.before.end);
        }
        if !h.after.is_empty() {
            edits.push(Edit::Insert {
                b: LineNo(lb),
                len: u64::from(h.after.end - h.after.start),
            });
            lb = u64::from(h.after.end);
        }
    }
    equal(&mut la, &mut lb, MemLines::new(a).line_count(), &mut edits);
    vec![hunk(edits)]
}

/// A fresh directory under the system temp dir.
fn temp_dir() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "rift-unified-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Output of the system `diff -U<ctx> -L a -L b`.
fn system_diff(a: &[u8], b: &[u8], ctx: u64) -> Vec<u8> {
    let dir = temp_dir();
    fs::write(dir.join("a"), a).unwrap();
    fs::write(dir.join("b"), b).unwrap();
    let out = Command::new("diff")
        .args([&format!("-U{ctx}"), "-L", "a", "-L", "b", "a", "b"])
        .current_dir(&dir)
        .output()
        .unwrap();
    fs::remove_dir_all(&dir).ok();
    assert!(
        out.status.code().is_some_and(|c| c < 2),
        "diff failed: {out:?}"
    );
    out.stdout
}

#[test]
fn matches_system_diff_on_corpus() {
    const A14: &[u8] = b"a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\nm\nn\n";
    let corpus: [(&[u8], &[u8], u64); 18] = [
        (b"", b"", 3),
        (b"", b"a\nb\n", 3),
        (b"a\nb\n", b"", 3),
        (b"a\nb\n", b"a\nb\n", 3),
        (b"a\n", b"b\n", 3),
        (b"a\nb", b"a\nb\nc", 3),
        (b"a\nb\n", b"a\nb", 3),
        (b"a\nb", b"a\nc", 3),
        (b"a", b"b\na", 3),
        (b"a\r\nb\r\n", b"a\r\nB\r\n", 3),
        (b"a\nb\nc\nd\ne\n", b"X\nb\nc\nd\ne\n", 3),
        (b"a\nb\nc\nd\ne\n", b"a\nb\nc\nd\nX\n", 3),
        (b"a\nb\nc\nd\n", b"a\nX\nc\nY\n", 3),
        // Changes 6 equal lines apart (2 * context): one hunk.
        (A14, b"a\nX\nc\nd\ne\nf\ng\nh\nY\nj\nk\nl\nm\nn\n", 3),
        // 7 apart: two hunks.
        (A14, b"a\nX\nc\nd\ne\nf\ng\nh\ni\nY\nk\nl\nm\nn\n", 3),
        (A14, b"a\nX\nc\nd\ne\nf\ng\nh\nY\nj\nk\nl\nm\nn\n", 0),
        (A14, b"a\nX\nc\nd\ne\nf\ng\nh\nY\nj\nk\nl\nm\nn\n", 1),
        (A14, b"a\nX\nc\nd\ne\nf\ng\nh\nY\nj\nk\nl\nm\nn\n", 2),
    ];
    for (a, b, ctx) in corpus {
        let expected = system_diff(a, b, ctx);
        let (ours, verdict) = render(a, b, &script(a, b), &options(ctx));
        assert_eq!(
            String::from_utf8_lossy(&ours),
            String::from_utf8_lossy(&expected),
            "a={a:?} b={b:?} ctx={ctx}"
        );
        assert_eq!(verdict == Verdict::Different, !expected.is_empty());
    }
}

#[test]
fn equal_runs_split_across_hunks_still_merge() {
    let a = b"a\nb\nc\nd\ne\nf\ng\nh\n";
    let b = b"X\nb\nc\nd\ne\nf\ng\nY\n";
    let one = script(a, b);
    let mut split = Vec::new();
    for e in &one[0].edits {
        split.push(hunk(match *e {
            Edit::Equal { a, b, len } if len > 1 => vec![
                Edit::Equal { a, b, len: 1 },
                Edit::Equal {
                    a: a + 1,
                    b: b + 1,
                    len: len - 1,
                },
            ],
            e => vec![e],
        }));
    }
    assert_eq!(
        render(a, b, &one, &options(3)),
        render(a, b, &split, &options(3))
    );
}

#[test]
fn approx_hunks_carry_a_marker_after_the_header() {
    let a = b"a\nb\nc\n";
    let b = b"a\nX\nc\n";
    let mut hunks = script(a, b);
    let (plain, _) = render(a, b, &hunks, &options(3));
    hunks[0].approx = true;
    let (marked, _) = render(a, b, &hunks, &options(3));
    assert_eq!(
        String::from_utf8_lossy(&marked),
        "--- a\n+++ b\n@@ -1,3 +1,3 @@ approx\n a\n-b\n+X\n c\n"
    );
    assert_eq!(marked.len(), plain.len() + " approx".len());
}

#[test]
fn headers_carry_name_and_gnu_style_mtime() {
    let ts = |secs, utc_offset| Timestamp {
        secs,
        nanos: 123_456_789,
        utc_offset,
    };
    let opts = Options {
        context: 3,
        old: Label {
            name: b"old.txt",
            mtime: Some(ts(0, 0)),
        },
        new: Label {
            name: b"new.txt",
            mtime: Some(ts(1_000_000_000, -5 * 3600)),
        },
    };
    let (out, _) = render(b"a\n", b"b\n", &script(b"a\n", b"b\n"), &opts);
    assert!(out.starts_with(
        b"--- old.txt\t1970-01-01 00:00:00.123456789 +0000\n\
          +++ new.txt\t2001-09-08 20:46:40.123456789 -0500\n"
    ));
    let mut buf = Vec::new();
    write_timestamp(&mut buf, ts(0, 19_800)).unwrap();
    assert_eq!(buf, b"1970-01-01 05:30:00.123456789 +0530");
    assert_eq!(civil_from_days(-1), (1969, 12, 31));
    assert_eq!(civil_from_days(11_017), (2000, 3, 1));
}

#[test]
fn exit_codes_follow_diff() {
    assert_eq!(exit_code::<()>(&Ok(Verdict::Identical)), 0);
    assert_eq!(exit_code::<()>(&Ok(Verdict::Different)), 1);
    assert_eq!(exit_code(&Err(())), 2);
    let bad = [hunk(vec![Edit::Delete {
        a: LineNo(1),
        len: 1,
    }])];
    let r = write_diff(
        &MemLines::new(b"a\n"),
        &MemLines::new(b""),
        |i| Ok(bad.get(i)),
        &options(3),
        Vec::new(),
    );
    assert_eq!(exit_code(&r), 2);
}

/// `patch` binaries on this machine: the system one and GNU's `gpatch`.
fn patchers() -> &'static [&'static str] {
    static FOUND: OnceLock<Vec<&'static str>> = OnceLock::new();
    FOUND.get_or_init(|| {
        ["patch", "gpatch"]
            .into_iter()
            .filter(|p| Command::new(p).arg("--version").output().is_ok())
            .collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    #[test]
    fn patch_applies_our_diff_to_a_and_gets_b(case in cases(24, true)) {
        let opts = Options { context: 3, old: label("a.txt"), new: label("a.txt") };
        let (diff, _) = render(&case.a, &case.b, &case.hunks, &opts);
        for &patcher in patchers() {
            let dir = temp_dir();
            fs::write(dir.join("a.txt"), &case.a).unwrap();
            fs::write(dir.join("d.patch"), &diff).unwrap();
            let out = Command::new(patcher)
                .args(["-s", "a.txt", "d.patch"])
                .current_dir(&dir)
                .output()
                .unwrap();
            let patched = fs::read(dir.join("a.txt")).unwrap_or_default();
            fs::remove_dir_all(&dir).ok();
            prop_assert!(out.status.success(), "{patcher}: {out:?}\n{}", String::from_utf8_lossy(&diff));
            prop_assert_eq!(&patched, &case.b, "{}", String::from_utf8_lossy(&diff));
        }
    }
}

#[test]
fn change_block_prints_deletes_before_inserts_whatever_the_edit_order() {
    let hunks = [hunk(vec![
        Edit::Insert {
            b: LineNo(0),
            len: 1,
        },
        Edit::Delete {
            a: LineNo(0),
            len: 1,
        },
        Edit::Insert {
            b: LineNo(1),
            len: 1,
        },
    ])];
    let (out, _) = render(b"a\n", b"b\nc", &hunks, &options(3));
    assert_eq!(
        String::from_utf8_lossy(&out),
        "--- a\n+++ b\n@@ -1 +1,2 @@\n-a\n+b\n+c\n\\ No newline at end of file\n"
    );
}
