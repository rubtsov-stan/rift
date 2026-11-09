//! Key mode prints the same bytes on every run and on any number of
//! threads: where a pass ends depends on the input and `--mem` only.
//!
//! Each run reads its peak heap from `--stats`, so no test here reads a
//! counter of this process.

use std::fmt::Write;
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

/// A `pg_dump` pair keyed by `copy:id`: `tables` tables of `rows` rows,
/// each named in two blocks if `repeat`. B holds the blocks in reverse
/// order and changes every 7th row.
fn dump(tables: usize, rows: usize, repeat: bool) -> Pair {
    let write = |reverse: bool| {
        let mut blocks = Vec::new();
        for t in 0..tables {
            for r in 0..if repeat { 2 } else { 1 } {
                let mut s = format!("COPY public.t{t} (id, v) FROM stdin;\n");
                for id in r * rows..(r + 1) * rows {
                    let v = if reverse && id % 7 == 0 {
                        "changed"
                    } else {
                        "same"
                    };
                    writeln!(
                        s,
                        "{id}\t{v} padding padding padding padding padding padding padding padding"
                    )
                    .unwrap();
                }
                s.push_str("\\.\n\nSELECT 1;\n\n");
                blocks.push(s);
            }
        }
        if reverse {
            blocks.reverse();
        }
        blocks.concat()
    };
    let dir = tempfile::tempdir().unwrap();
    let (pa, pb) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::write(&pa, write(false)).unwrap();
    std::fs::write(&pb, write(true)).unwrap();
    Pair {
        _dir: dir,
        a: pa,
        b: pb,
    }
}

/// What a `--key copy:id --stats` run printed: exit code, standard
/// output, and standard error without the lines that measure the run,
/// after checking the peak heap against `--mem`.
fn keyed(pair: &Pair, mem: &str, threads: &str) -> (Option<i32>, Vec<u8>, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_rift"))
        .args(["--key", "copy:id", "--stats", "--mem", mem])
        .args([&pair.a, &pair.b])
        .env("RAYON_NUM_THREADS", threads)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let row = format!("--mem {mem}, {threads} threads");
    let peak: u64 = stderr
        .lines()
        .find_map(|l| l.strip_prefix("peak heap "))
        .and_then(|l| l.rsplit_once(" (")?.1.strip_suffix(" bytes)")?.parse().ok())
        .unwrap_or_else(|| panic!("{row}: no peak heap in {stderr}"));
    let limit = rift_core::Bytes::parse_budget(mem).unwrap().0;
    assert!(peak <= limit, "{row}: peak heap {peak} over {limit}");
    let mut rest = String::new();
    for l in stderr.lines() {
        if !l.starts_with("peak heap ") && !l.starts_with("time ") {
            writeln!(rest, "{l}").unwrap();
        }
    }
    (out.status.code(), out.stdout, rest)
}

/// The `--mem` a refusal advises.
fn advised(stderr: &str) -> Option<&str> {
    let (_, mem) = stderr.split_once("; rerun with --mem ")?;
    mem.split_whitespace().next()
}

/// Each budget prints the same bytes, error included, on every run and
/// thread count; a refusal always advises a `--mem`, at least half as much
/// again, and following the advice gets a full answer within `steps`
/// reruns.
fn every_budget_answers_or_advises(
    pair: &Pair,
    budgets: &[&str],
    threads: &[&str],
    runs: usize,
    steps: usize,
) {
    for &mem in budgets {
        let first = keyed(pair, mem, threads[0]);
        for &t in threads {
            for i in 0..runs {
                let next = keyed(pair, mem, t);
                assert!(
                    next == first,
                    "--mem {mem}: run {i} on {t} threads differs from {} thread: {:?} vs {:?}",
                    threads[0],
                    next.2,
                    first.2
                );
            }
        }
        let (mut code, mut at, mut stderr) = (first.0, mem.to_owned(), first.2);
        let mut reruns = 0;
        while code != Some(1) {
            assert_eq!(code, Some(2), "--mem {at}: {stderr}");
            let next = advised(&stderr)
                .unwrap_or_else(|| panic!("--mem {at} refused with no advice: {stderr}"))
                .to_owned();
            let (from, to) = (bytes(&at), bytes(&next));
            assert!(to >= from + from / 2, "--mem {at} advised {next}");
            reruns += 1;
            assert!(
                reruns <= steps,
                "--mem {mem}: no answer after {steps} reruns, at {next}"
            );
            (code, _, stderr) = keyed(pair, &next, threads[threads.len() - 1]);
            at = next;
        }
    }
}

fn bytes(mem: &str) -> u64 {
    rift_core::Bytes::parse_budget(mem).unwrap().0
}

/// 1200 tables, and 300 tables named twice each. Both used to be refused
/// at 5M with no advice, and at 2M with advice of 2049K, which is refused
/// again. 256K is too small for either.
#[test]
fn copy_dumps_answer_or_advise_the_same_way_every_run() {
    for pair in [dump(1200, 15, false), dump(300, 30, true)] {
        every_budget_answers_or_advises(&pair, &["256K", "2M", "5M"], &["1", "8"], 1, 3);
    }
}

#[test]
#[ignore = "about 2000 runs; run with --release"]
fn copy_dumps_answer_or_advise_the_same_way_on_any_thread_count() {
    let budgets = [
        "256K", "512K", "1M", "2M", "3M", "4M", "4352K", "5M", "6M", "7M", "8M", "10M", "12M",
        "14M", "16M",
    ];
    for pair in [dump(1200, 30, false), dump(600, 30, true)] {
        every_budget_answers_or_advises(&pair, &budgets, &["1", "2", "8", "64"], 20, 3);
    }
}
