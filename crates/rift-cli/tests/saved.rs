//! `--save`, `rift info` and `rift open` from the command line.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rift_gen::{Config, Profile};

fn rift(args: &[&Path], env: Option<(&str, &str)>) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rift"));
    cmd.args(args);
    if let Some((k, v)) = env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

struct Pair {
    dir: tempfile::TempDir,
    a: PathBuf,
    b: PathBuf,
}

fn pair(size: u64) -> Pair {
    let mut config = Config::new(7, size, Profile::Sql);
    config.change_ratio = 0.01;
    let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
    rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (pa, pb) = (dir.path().join("a.sql"), dir.path().join("b.sql"));
    std::fs::write(&pa, a).unwrap();
    std::fs::write(&pb, b).unwrap();
    Pair { dir, a: pa, b: pb }
}

fn p(s: &str) -> &Path {
    Path::new(s)
}

#[test]
fn saving_leaves_the_output_and_exit_code_alone() {
    let t = pair(2 << 20);
    let r = t.dir.path().join("r.riftdiff");
    let plain = rift(&[&t.a, &t.b], None);
    let saved = rift(&[&t.a, &t.b, p("--save"), &r], None);
    assert_eq!(plain.status.code(), Some(1));
    assert_eq!(
        saved.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&saved.stderr)
    );
    assert!(plain.stdout == saved.stdout, "--save changed the diff");
    assert!(r.exists());
    assert!(!t.dir.path().join("r.riftdiff.tmp").exists());

    let info = rift(&[p("info"), &r], None);
    assert_eq!(info.status.code(), Some(0));
    let text = String::from_utf8(info.stdout).unwrap();
    assert!(text.contains("a.sql") && text.contains(": fresh"), "{text}");
    assert!(text.contains(", complete"), "{text}");
}

#[test]
fn the_result_is_the_same_on_one_core_and_on_four() {
    // Several scan chunks per file, so hash chunks cross them.
    let t = pair(10 << 20);
    let (r1, r4) = (
        t.dir.path().join("1.riftdiff"),
        t.dir.path().join("4.riftdiff"),
    );
    let one = rift(&[&t.a, &t.b, p("--save"), &r1], Some(("RIFT_CORES", "1")));
    let four = rift(&[&t.a, &t.b, p("--save"), &r4], Some(("RIFT_CORES", "4")));
    assert_eq!(one.status.code(), Some(1));
    assert_eq!(one.stdout, four.stdout);
    let (b1, b4) = (std::fs::read(&r1).unwrap(), std::fs::read(&r4).unwrap());
    // Chunk table A starts right after the 64-byte header.
    let chunks = 10;
    assert!(b1.len() > 64 + 8 * chunks);
    assert!(b1 == b4, "the results differ");
}

#[test]
fn info_tells_a_touched_file_from_one_that_changed_size() {
    let t = pair(1 << 20);
    let r = t.dir.path().join("r.riftdiff");
    assert_eq!(
        rift(&[&t.a, &t.b, p("--save"), &r], None).status.code(),
        Some(1)
    );
    let mut b = std::fs::read(&t.b).unwrap();
    b[10] ^= 1;
    std::fs::remove_file(&t.b).unwrap();
    std::fs::write(&t.b, &b).unwrap();
    let text = String::from_utf8(rift(&[p("info"), &r], None).stdout).unwrap();
    assert!(
        text.contains("b: ") && text.contains("unverified"),
        "{text}"
    );
    b.pop();
    std::fs::write(&t.b, &b).unwrap();
    let text = String::from_utf8(rift(&[p("info"), &r], None).stdout).unwrap();
    assert!(text.contains("stale"), "{text}");
}

#[test]
fn a_summary_save_holds_the_map_alone() {
    let t = pair(1 << 20);
    let r = t.dir.path().join("r.riftdiff");
    let out = rift(&[&t.a, &t.b, p("--summary"), p("--save"), &r], None);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(rift(&[p("info"), &r], None).stdout).unwrap();
    assert!(
        text.contains("scripts  0 of") && text.contains("partial"),
        "{text}"
    );
}

#[test]
fn what_cannot_be_saved_or_opened_is_refused() {
    let t = pair(64 << 10);
    let r = t.dir.path().join("r.riftdiff");
    let dirs = rift(&[t.dir.path(), t.dir.path(), p("--save"), &r], None);
    assert_eq!(dirs.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&dirs.stderr).contains("--save"));
    let garbage = rift(&[p("info"), &t.a], None);
    assert_eq!(garbage.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&garbage.stderr).contains("not a Rift result"));
    assert_eq!(
        rift(&[&t.a, &t.b, p("--save"), &r], None).status.code(),
        Some(1)
    );
    let open = rift(&[p("open"), &r], None);
    assert_eq!(open.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&open.stderr).contains("terminal"));
}
