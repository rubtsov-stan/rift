//! Key mode prints the same bytes on every run and on any number of
//! threads: where a pass ends depends on the input and `--mem` only.
//!
//! Each run reads its peak heap from `--stats`, so no test here reads a
//! counter of this process.

use std::path::PathBuf;
use std::process::Command;

use rift_gen::{Config, Profile, Rows};

const THREADS: [&str; 5] = ["1", "2", "4", "8", "32"];

struct Pair {
    _dir: tempfile::TempDir,
    a: PathBuf,
    b: PathBuf,
}

/// A shuffled CSV pair of `size` bytes, as `rift-gen --profile shuffled
/// --seed 7 --change-ratio 0.01` writes it.
fn shuffled(size: u64) -> Pair {
    let mut config = Config::new(7, size, Profile::Shuffled);
    config.rows = Rows::Csv;
    config.change_ratio = 0.01;
    let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
    rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
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

/// Standard output of `rift --key csv:id --stats --mem MEM a b` on
/// `threads` threads, after checking that it found changes and peaked
/// under `--mem`.
fn run(pair: &Pair, mem: &str, threads: &str, records: bool) -> Vec<u8> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rift"));
    cmd.args(["--key", "csv:id", "--stats", "--mem", mem]);
    if records {
        cmd.arg("--show-records");
    }
    let out = cmd
        .args([&pair.a, &pair.b])
        .env("RAYON_NUM_THREADS", threads)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let row = format!("--mem {mem}, {threads} threads, records {records}");
    assert_eq!(out.status.code(), Some(1), "{row}: {stderr}");
    let peak: u64 = stderr
        .lines()
        .find_map(|l| l.strip_prefix("peak heap "))
        .and_then(|l| l.rsplit_once(" (")?.1.strip_suffix(" bytes)")?.parse().ok())
        .unwrap_or_else(|| panic!("{row}: no peak heap in {stderr}"));
    let limit = rift_core::Bytes::parse_budget(mem).unwrap().0;
    assert!(peak <= limit, "{row}: peak heap {peak} over {limit}");
    out.stdout
}

/// Runs every budget on every thread count `runs` times, with and
/// without `--show-records`, and checks that each budget prints one output
/// whatever the run and the threads, and that every budget prints the
/// same lines in some order.
fn same_bytes_every_run(pair: &Pair, budgets: &[&str], runs: usize) {
    for records in [false, true] {
        let mut lines: Option<Vec<Vec<u8>>> = None;
        for &mem in budgets {
            let first = run(pair, mem, THREADS[0], records);
            for threads in THREADS {
                for i in 0..runs {
                    assert!(
                        run(pair, mem, threads, records) == first,
                        "--mem {mem}, records {records}: run {i} on {threads} threads differs from {} thread",
                        THREADS[0]
                    );
                }
            }
            let mut sorted: Vec<Vec<u8>> =
                first.split(|&c| c == b'\n').map(<[u8]>::to_vec).collect();
            sorted.sort_unstable();
            match &lines {
                None => lines = Some(sorted),
                Some(want) => assert!(
                    *want == sorted,
                    "--mem {mem}, records {records}: other lines than --mem {}",
                    budgets[0]
                ),
            }
        }
    }
}

/// A 16 MiB pair takes two passes at both budgets, on 3 shards at 8M and
/// on 8 at 17M; on 0.3.1 it printed two orders at 17M, by thread count.
#[test]
fn the_output_does_not_depend_on_the_thread_count() {
    let pair = shuffled(16 << 20);
    same_bytes_every_run(&pair, &["8M", "17M"], 1);
}

#[test]
#[ignore = "about 1200 runs on a 32 MiB pair; run with --release"]
fn every_run_prints_the_same_bytes() {
    let pair = shuffled(32 << 20);
    same_bytes_every_run(&pair, &["4M", "16M", "17M", "24M", "32M", "256M"], 20);
}
