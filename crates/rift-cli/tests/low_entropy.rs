//! Low-entropy pairs end to end: the diff patches A into B and the peak
//! heap that `--stats` reports stays within `--mem`.

use std::fs;
use std::path::Path;
use std::process::Command;

use rift_gen::{Config, Profile};

const MEM: u64 = 16 << 20;

/// Every point of the low-entropy profiles: the profile and its
/// `--distinct` or `--period`.
const POINTS: [(Profile, u64); 10] = [
    (Profile::Distinct, 1),
    (Profile::Distinct, 2),
    (Profile::Distinct, 10),
    (Profile::Distinct, 100),
    (Profile::Distinct, 10_000),
    (Profile::Distinct, 1_000_000),
    (Profile::Periodic, 1000),
    (Profile::Runs, 0),
    (Profile::Alternating, 0),
    (Profile::Empty, 0),
];

/// The exact byte count from `peak heap X of Y (N bytes)`.
fn peak_bytes(stderr: &str) -> u64 {
    let line = stderr
        .lines()
        .find(|l| l.starts_with("peak heap"))
        .expect("--stats prints the peak heap");
    let n = line.rsplit_once('(').unwrap().1;
    n.trim_end_matches(" bytes)").parse().unwrap()
}

/// Applies `diff` to a copy of `a` with the system `patch` and checks the
/// result is `b`.
fn patch_gives_b(dir: &Path, diff: &[u8], b: &[u8]) {
    fs::copy(dir.join("a"), dir.join("target")).unwrap();
    fs::write(dir.join("d.patch"), diff).unwrap();
    let out = Command::new("patch")
        .args(["-s", "-o", "out", "target", "d.patch"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "patch: {out:?}");
    assert!(
        fs::read(dir.join("out")).unwrap() == b,
        "patch did not produce B"
    );
}

#[test]
fn low_entropy_pairs_patch_to_b_within_mem() {
    for (profile, k) in POINTS {
        let mut config = Config::new(7, 4 << 20, profile);
        config.distinct = k;
        config.period = k;
        let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
        rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
        assert_ne!(a, b, "{profile:?} {k}");
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a"), &a).unwrap();
        fs::write(dir.path().join("b"), &b).unwrap();
        let out = Command::new(env!("CARGO_BIN_EXE_rift"))
            .args(["--stats", "--mem", "16M", "a", "b"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{profile:?} {k}: {stderr}");
        let peak = peak_bytes(&stderr);
        assert!(peak <= MEM, "{profile:?} {k}: peak heap {peak} over {MEM}");
        patch_gives_b(dir.path(), &out.stdout, &b);
    }
}
