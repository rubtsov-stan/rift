//! The output depends only on `--mem` and the input: the plain diff,
//! `--summary`, `--stats`, key mode and `rift serve` print the same bytes
//! under every `RAYON_NUM_THREADS`, and as a machine of 4 or 1 cores, which
//! `RIFT_CORES` stands in for. The budgets include the points where the
//! output used to flip with the thread count.
#![allow(
    clippy::many_single_char_names,
    reason = "c is the client, s a session, a and b the files, as in the protocol"
)]

#[path = "serve/client.rs"]
#[allow(dead_code, reason = "the serve tests use the rest of the client")]
mod client;

use std::path::{Path, PathBuf};
use std::process::Command;

use client::Client;
use rift_gen::{Config, Profile};
use serde_json::{Value, json};

/// The machines to compare: pool sizes, and core counts with the pool left
/// at its default.
const POOLS: [(&str, &str); 6] = [
    ("RAYON_NUM_THREADS", "1"),
    ("RAYON_NUM_THREADS", "4"),
    ("RAYON_NUM_THREADS", "8"),
    ("RAYON_NUM_THREADS", "64"),
    ("RIFT_CORES", "4"),
    ("RIFT_CORES", "1"),
];

struct Pair {
    _dir: tempfile::TempDir,
    a: PathBuf,
    b: PathBuf,
}

fn pair(a: &[u8], b: &[u8]) -> Pair {
    let dir = tempfile::tempdir().unwrap();
    let (pa, pb) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::write(&pa, a).unwrap();
    std::fs::write(&pb, b).unwrap();
    Pair {
        _dir: dir,
        a: pa,
        b: pb,
    }
}

fn generated(seed: u64, size: u64, profile: Profile, change_ratio: f64) -> Pair {
    let mut config = Config::new(seed, size, profile);
    config.change_ratio = change_ratio;
    let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
    rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
    pair(&a, &b)
}

/// 300 000 equal lines, one of them changed in B: one segment whose
/// estimate lies close to the scheduler's share near 187.5 MiB.
fn one_change() -> Pair {
    let a = "same line\n".repeat(300_000);
    let mut b = a.clone();
    b.replace_range(1_500_000..1_500_010, "other line\n");
    pair(a.as_bytes(), b.as_bytes())
}

/// Runs `rift args` with the environment variable `pool` and returns the exit
/// code, standard output and standard error, without the lines that hold
/// the time and the peak heap, which are measured, not computed.
fn rift(args: &[&str], files: &Pair, pool: (&str, &str)) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_rift"))
        .args(args)
        .arg(&files.a)
        .arg(&files.b)
        .env_remove("RAYON_NUM_THREADS")
        .env_remove("RIFT_CORES")
        .env(pool.0, pool.1)
        .output()
        .unwrap();
    let measured = |l: &&str| !l.starts_with("peak heap") && !l.starts_with("time ");
    let keep = |bytes: &[u8]| {
        String::from_utf8_lossy(bytes)
            .lines()
            .filter(measured)
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "exit {:?}\n{}\n--- stderr\n{}",
        out.status.code(),
        keep(&out.stdout),
        keep(&out.stderr)
    )
}

/// Checks that every pool of [`POOLS`] prints the same for `args` under
/// each budget of `mems`.
fn same_for_every_pool(args: &[&str], files: &Pair, mems: &[&str]) {
    for mem in mems {
        let mut args = args.to_vec();
        args.extend(["--mem", mem]);
        let first = rift(&args, files, POOLS[0]);
        for pool in &POOLS[1..] {
            let other = rift(&args, files, *pool);
            assert!(
                first == other,
                "{args:?}: {:?} and {pool:?} differ:\n{}\n=====\n{}",
                POOLS[0],
                first.chars().take(2000).collect::<String>(),
                other.chars().take(2000).collect::<String>()
            );
        }
    }
}

/// What `rift serve` answers about a pair: the summary without the bytes
/// reserved, which count the workers that started, every segment and
/// every differing segment's hunk, and the moved blocks.
fn served(files: &Pair, mem: &str, pool: (&str, &str)) -> Value {
    let path = |p: &Path| p.to_str().unwrap().to_owned();
    let mut c = Client::start_with_env(&[], &[pool]);
    c.initialize();
    let opened = c.ok(
        "open",
        json!({"a": path(&files.a), "b": path(&files.b), "mem": mem}),
    );
    let s = opened["session"].as_u64().unwrap();
    assert_eq!(c.state_of(s)["state"], "ready");
    c.until("refinement to finish", |c| {
        c.notes_of("progress", s)
            .iter()
            .any(|p| p["phase"] == "refine" && p["done"] == p["total"])
    });
    let mut summary = c.ok("summary", json!({"session": s}));
    summary.as_object_mut().unwrap().remove("reserved");
    let segments = c.ok("segments", json!({"session": s, "limit": 1000}));
    let hunks: Vec<Value> = segments["segments"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|g| g["status"] != "equal")
        .map(|g| c.ok("hunk", json!({"session": s, "segment": g["index"]})))
        .collect();
    let moves = c.ok("moves", json!({"session": s, "limit": 1000}));
    let (status, stderr) = c.finish();
    assert_eq!(status.code(), Some(0), "{stderr}");
    json!({"summary": summary, "segments": segments, "hunks": hunks, "moves": moves})
}

#[test]
fn a_segment_near_the_scheduler_share_is_refined_the_same_way() {
    // At 192000K, 64 threads used to leave 750 KiB less budget than one,
    // and the segment came out approximate.
    same_for_every_pool(&[], &one_change(), &["192000K"]);
}

#[test]
fn plain_summary_and_stats_do_not_follow_the_pool() {
    let files = generated(5, 4 << 20, Profile::Nokey, 0.2);
    same_for_every_pool(&[], &files, &["11M", "24M"]);
    same_for_every_pool(&["--stats"], &files, &["12M", "16M"]);
    same_for_every_pool(&["--summary"], &files, &["1M", "4M", "11M"]);
}

#[test]
fn key_mode_does_not_follow_the_pool() {
    let files = generated(3, 2 << 20, Profile::Csv, 0.05);
    same_for_every_pool(&["--key", "1"], &files, &["2M", "4M", "8M"]);
}

#[test]
fn serve_does_not_follow_the_pool() {
    let files = generated(5, 2 << 20, Profile::Nokey, 0.2);
    let first = served(&files, "16M", POOLS[0]);
    for pool in &POOLS[1..] {
        assert_eq!(first, served(&files, "16M", *pool), "{pool:?}");
    }
}

/// Pairs of three-line runs swapped in B, then a large region with no
/// line in common: tens of thousands of moved blocks.
fn swaps(swaps: u64, big: u64) -> Pair {
    use std::fmt::Write;
    let (mut a, mut b) = (String::new(), String::new());
    for s in 0..swaps {
        let run = |k: u64| -> String {
            (0..3).fold(String::new(), |mut t, i| {
                writeln!(t, "row-{:09},alpha", 6 * s + k + i).unwrap();
                t
            })
        };
        let (x, y) = (run(0), run(3));
        writeln!(a, "{x}{y}sep-{s:09}").unwrap();
        writeln!(b, "{y}{x}sep-{s:09}").unwrap();
    }
    for i in 0..big {
        writeln!(a, "old-{i:09},{}", "z".repeat(20)).unwrap();
        writeln!(b, "new-{i:09},{}", "q".repeat(20)).unwrap();
    }
    for i in 0..1000 {
        writeln!(a, "tail-{i}").unwrap();
        writeln!(b, "tail-{i}").unwrap();
    }
    pair(a.as_bytes(), b.as_bytes())
}

/// The review's reproducers, at the budgets where their output flipped
/// with the thread count, and a sweep around them.
#[test]
#[ignore = "generates pairs of 60 to 64 MiB; run in release"]
fn the_reproducers_do_not_follow_the_pool() {
    let modes: [&[&str]; 2] = [&["--stats"], &["--summary"]];
    let nokey = generated(5, 64 << 20, Profile::Nokey, 0.001);
    for args in modes {
        same_for_every_pool(args, &nokey, &["27295K", "27000K", "27600K", "32M"]);
    }
    let sql = generated(7, 64 << 20, Profile::Sql, 0.001);
    for args in modes {
        same_for_every_pool(args, &sql, &["11M", "12M", "16M", "24M"]);
    }
    let changed = generated(7, 60 << 20, Profile::Nokey, 0.2);
    for args in modes {
        same_for_every_pool(args, &changed, &["10892K", "11M", "12M"]);
    }
    // A `--summary` once seen to differ on this pair at the default
    // --mem; run it a few times over.
    let default = generated(7, 64 << 20, Profile::Nokey, 0.001);
    for _ in 0..3 {
        same_for_every_pool(&["--summary"], &default, &["256M"]);
    }
    let moved = swaps(60_000, 1000);
    for args in modes {
        same_for_every_pool(args, &moved, &["48M", "49M", "64M"]);
    }
}
