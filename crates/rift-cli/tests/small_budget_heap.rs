//! At the smallest `--mem` Rift accepts, at 1M and at 4M, the peak heap of
//! the whole process stays at or under `--mem` in every mode, refusals and
//! error exits included.
//!
//! Each run reads the peak that the binary's own counting allocator prints
//! under `--stats`, so no test here reads a counter of this process, and
//! the tests share one binary and run in parallel. A new mode is one more
//! call to [`check`]. Every run goes twice: with rayon's default pool and
//! with `RAYON_NUM_THREADS` well above the cores, since part of the heap
//! no budget counts grows with the pool.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use rift_gen::{Config, Profile, Rows};

const BUDGETS: [&str; 4] = ["64K", "1M", "2M", "4M"];

struct Pair {
    _dir: tempfile::TempDir,
    a: PathBuf,
    b: PathBuf,
}

fn generated(size: u64, profile: Profile, rows: Rows) -> Pair {
    let mut config = Config::new(7, size, profile);
    config.rows = rows;
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

fn bytes(mem: &str) -> u64 {
    rift_core::Bytes::parse_budget(mem).unwrap().0
}

/// Pool sizes to run with: rayon's default, and many more threads than
/// this machine has cores.
const POOLS: [Option<&str>; 2] = [None, Some("256")];

/// Runs `rift` with `args` under `--mem mem`, once per pool of [`POOLS`],
/// and checks that it ends as `diff` does or with an error that `allow`
/// accepts. Returns a line for each peak heap over `--mem`.
fn check(args: &[&OsStr], mem: &str, allow: fn(&str) -> bool) -> Vec<String> {
    POOLS
        .into_iter()
        .filter_map(|pool| check_with(args, mem, allow, pool))
        .collect()
}

fn check_with(
    args: &[&OsStr],
    mem: &str,
    allow: fn(&str) -> bool,
    pool: Option<&str>,
) -> Option<String> {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rift"));
    cmd.args(args).env_remove("RAYON_NUM_THREADS");
    if let Some(n) = pool {
        cmd.env("RAYON_NUM_THREADS", n);
    }
    let out = cmd.output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let code = out.status.code();
    assert!(
        matches!(code, Some(0 | 1)) || (code == Some(2) && allow(&stderr)),
        "{args:?} pool {pool:?}: exit {code:?}, {stderr}"
    );
    let report = [String::from_utf8_lossy(&out.stdout), stderr].concat();
    let peak: u64 = report
        .lines()
        .find_map(|l| l.strip_prefix("peak heap "))
        .and_then(|l| l.rsplit_once(" (")?.1.strip_suffix(" bytes)")?.parse().ok())
        .unwrap_or_else(|| panic!("{args:?}: no peak heap in {report}"));
    eprintln!("{args:?} pool {pool:?}: exit {code:?}, peak heap {peak}");
    (peak > bytes(mem)).then(|| {
        format!(
            "{args:?} pool {pool:?}: peak heap {peak} over {}",
            bytes(mem)
        )
    })
}

fn refused(stderr: &str) -> bool {
    stderr.contains(" budget exceeded: ")
}

/// `rift OPTS --stats --mem MEM a b` at every budget.
fn diff_peaks_under_mem(opts: &[&str], a: &Path, b: &Path) {
    let over: Vec<_> = BUDGETS
        .into_iter()
        .flat_map(|mem| {
            let mut args: Vec<&OsStr> = opts.iter().map(OsStr::new).collect();
            args.extend(["--stats", "--mem", mem].map(OsStr::new));
            args.extend([a.as_os_str(), b.as_os_str()]);
            check(&args, mem, refused)
        })
        .collect();
    assert!(over.is_empty(), "{over:#?}");
}

#[test]
fn the_full_diff_peaks_under_mem() {
    let p = generated(2 << 20, Profile::Sql, Rows::Csv);
    diff_peaks_under_mem(&[], &p.a, &p.b);
}

#[test]
fn the_summary_peaks_under_mem() {
    let p = generated(2 << 20, Profile::Sql, Rows::Csv);
    diff_peaks_under_mem(&["--summary"], &p.a, &p.b);
}

/// Each budget gets a pair four times its size, so that key mode needs
/// more than one pass and fills both halves. A key-mode option is one
/// more row.
#[test]
fn key_mode_peaks_under_mem() {
    let mut over = Vec::new();
    for (opts, rows) in [
        (&["--key", "csv:id"][..], Rows::Csv),
        (&["--key", "json:id"], Rows::Ndjson),
        (&["--key", "copy:id"], Rows::Sql),
        (&["--key", "csv:id", "--show-records"], Rows::Csv),
    ] {
        for mem in BUDGETS {
            let p = generated(4 * bytes(mem), Profile::Shuffled, rows);
            let mut args: Vec<&OsStr> = opts.iter().map(OsStr::new).collect();
            args.extend(["--stats", "--mem", mem].map(OsStr::new));
            args.extend([p.a.as_os_str(), p.b.as_os_str()]);
            over.extend(check(&args, mem, refused));
        }
    }
    assert!(over.is_empty(), "{over:#?}");
}

/// At 64K a 1 MiB pair would take over a hundred passes, more than key
/// mode runs: the refusal names a `--mem`, and a run at that limit
/// answers. Both stay under their limit, with either pool.
#[test]
fn key_mode_refuses_too_many_passes_and_advises_a_mem_that_works() {
    let mut over = Vec::new();
    for (opts, rows) in [
        (&["--key", "csv:id"][..], Rows::Csv),
        (&["--key", "json:id"], Rows::Ndjson),
        (&["--key", "copy:id"], Rows::Sql),
    ] {
        let p = generated(1 << 20, Profile::Shuffled, rows);
        let args = |mem: &str| -> Vec<OsString> {
            let mut args: Vec<OsString> = opts.iter().map(OsString::from).collect();
            args.extend(["--stats", "--mem", mem].map(OsString::from));
            args.extend([p.a.clone().into_os_string(), p.b.clone().into_os_string()]);
            args
        };
        let out = Command::new(env!("CARGO_BIN_EXE_rift"))
            .args(args("64K"))
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{opts:?}: {stderr}");
        assert!(stderr.starts_with("rift: "), "{opts:?}: {stderr}");
        assert!(refused(&stderr), "{opts:?}: {stderr}");
        let (_, advised) = stderr
            .split_once("; rerun with --mem ")
            .unwrap_or_else(|| panic!("{opts:?}: no advice in {stderr}"));
        let advised = advised.split_whitespace().next().unwrap().to_owned();
        assert!(bytes(&advised) > bytes("64K"), "{opts:?}: {advised}");
        for (mem, allow) in [
            ("64K", refused as fn(&str) -> bool),
            (&advised, |_: &str| false),
        ] {
            let args = args(mem);
            let args: Vec<&OsStr> = args.iter().map(OsString::as_os_str).collect();
            over.extend(check(&args, mem, allow));
        }
    }
    assert!(over.is_empty(), "{over:#?}");
}

#[test]
fn git_diff_peaks_under_mem() {
    let p = generated(2 << 20, Profile::Sql, Rows::Csv);
    let over: Vec<_> = BUDGETS
        .into_iter()
        .flat_map(|mem| {
            let mut args: Vec<&OsStr> = ["git-diff", "--stats", "--mem", mem, "f"]
                .map(OsStr::new)
                .to_vec();
            args.extend([p.a.as_os_str(), "1111111".as_ref(), "100644".as_ref()]);
            args.extend([p.b.as_os_str(), "2222222".as_ref(), "100644".as_ref()]);
            check(&args, mem, refused)
        })
        .collect();
    assert!(over.is_empty(), "{over:#?}");
}

#[test]
fn a_missing_file_peaks_under_mem() {
    let p = generated(64 << 10, Profile::Sql, Rows::Csv);
    let missing = p.a.with_file_name("missing");
    let over: Vec<_> = BUDGETS
        .into_iter()
        .flat_map(|mem| {
            let mut args: Vec<&OsStr> = ["--stats", "--mem", mem].map(OsStr::new).to_vec();
            args.extend([p.a.as_os_str(), missing.as_os_str()]);
            check(&args, mem, |e| e.contains("missing"))
        })
        .collect();
    assert!(over.is_empty(), "{over:#?}");
}

/// Below the smallest budget the process cannot keep its own start within
/// `--mem`, so the value is refused before anything runs.
#[test]
fn a_mem_below_the_smallest_is_refused() {
    let p = generated(64 << 10, Profile::Sql, Rows::Csv);
    for mem in ["1", "1K", "63K"] {
        let out = Command::new(env!("CARGO_BIN_EXE_rift"))
            .args(["--mem", mem])
            .arg(&p.a)
            .arg(&p.b)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "--mem {mem}: {stderr}");
        assert!(stderr.contains("at least 64K"), "--mem {mem}: {stderr}");
    }
}
