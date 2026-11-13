//! A budget refusal names a `--mem` that gets past it, in every mode.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn rift(args: &[&str], mem: &str, a: &Path, b: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rift"))
        .args(args)
        .args(["--mem", mem])
        .arg(a)
        .arg(b)
        .output()
        .unwrap()
}

fn pair(dir: &Path, a: &str, b: &str) -> (PathBuf, PathBuf) {
    let (pa, pb) = (dir.join("a"), dir.join("b"));
    std::fs::write(&pa, a).unwrap();
    std::fs::write(&pb, b).unwrap();
    (pa, pb)
}

fn text(changed: bool) -> String {
    (0..2000)
        .map(|i| {
            if changed && i % 100 == 0 {
                format!("changed {i}\n")
            } else {
                format!("row {i}\n")
            }
        })
        .collect()
}

/// The `--mem` a refusal advises, if it is one.
fn advice(out: &Output) -> Option<String> {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let (_, mem) = stderr.split_once("; rerun with --mem ")?;
    Some(mem.trim_end().to_owned())
}

fn bytes(mem: &str) -> u64 {
    rift_core::Bytes::parse_budget(mem).unwrap().0
}

/// Follows the advice from `mem` until a run gets past the budget, and
/// returns the refusals on the way and the last run. Each refusal exits 2
/// and advises more than the limit it was given.
fn follow(args: &[&str], mut mem: String, a: &Path, b: &Path) -> (Vec<String>, Output) {
    let mut refused = Vec::new();
    for _ in 0..32 {
        let out = rift(args, &mem, a, b);
        let Some(next) = advice(&out) else {
            return (refused, out);
        };
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(2), "{stderr}");
        assert!(stderr.starts_with("rift: "), "{stderr}");
        assert!(stderr.contains(" budget exceeded: requested "), "{stderr}");
        assert!(bytes(&next) > bytes(&mem), "{mem} advised {next}");
        refused.push(mem);
        mem = next;
    }
    panic!("{args:?}: no --mem in 32 steps gets past the budget");
}

#[test]
fn summary_and_full_diff_advise_a_mem_that_works() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path(), &text(false), &text(true));
    for args in [&["--summary"][..], &[]] {
        let (refused, last) = follow(args, "64K".into(), &a, &b);
        assert!(!refused.is_empty(), "{args:?}: 64K is too small");
        assert_eq!(
            last.status.code(),
            Some(1),
            "{args:?}: {}",
            String::from_utf8_lossy(&last.stderr)
        );
    }
}

/// From just under what one worker needs, one step is enough.
#[test]
fn a_full_diff_short_of_one_worker_needs_one_rerun() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path(), &text(false), &text(true));
    let (refused, last) = follow(&[], "8M".into(), &a, &b);
    assert_eq!(refused, ["8M"]);
    assert_eq!(last.status.code(), Some(1));
}

#[test]
fn key_mode_advises_a_mem_that_works() {
    let dir = tempfile::tempdir().unwrap();
    // Records of 8 KiB: key mode keeps only the base reserve, and 64K
    // holds a few thousand short ones.
    let rows = |v: &str| -> String {
        std::iter::once("id,v\n".to_owned())
            .chain((0..50).map(|i| format!("{i},{}{i}\n", v.repeat(8 << 10))))
            .collect()
    };
    let (a, b) = pair(dir.path(), &rows("x"), &rows("y"));
    let (refused, last) = follow(&["--key", "csv:id"], "64K".into(), &a, &b);
    assert!(!refused.is_empty(), "64K is too small");
    assert_eq!(
        last.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&last.stderr)
    );
}

/// Bytes a refusal says were requested.
fn requested(out: &Output) -> u64 {
    let stderr = String::from_utf8_lossy(&out.stderr);
    let (_, rest) = stderr.split_once(" requested ").expect("a budget refusal");
    rest.split_once(' ').unwrap().0.parse().unwrap()
}

/// A record larger than the budget allows is read to its end, and the
/// advice is enough for it: one rerun at the advised value succeeds.
/// From the smallest --mem, a few refusals for the readers' smallest
/// windows come first, then one for the record. Every --mem here is below
/// the threshold for more than one thread.
#[test]
fn a_long_record_needs_one_rerun() {
    let long = "x".repeat(150_000);
    let quoted = format!("\"{}\"", "a,b\n".repeat(40_000));
    let cases = [
        (
            "csv:id",
            format!("id,v\n1,{quoted}\n2,y\n3,z\n"),
            "id,v\n3,z\n2,y\n1,\"short\"\n".to_owned(),
            "~1\t2\t4\n",
        ),
        (
            "csv:id",
            format!("id,v\n1,{long}\n2,y\n"),
            format!("id,v\n2,y\n1,{long}z\n"),
            "~1\t2\t3\n",
        ),
        (
            "json:id",
            format!("{{\"id\":1,\"v\":\"{long}\"}}\n{{\"id\":2}}\n"),
            "{\"id\":2}\n{\"id\":1,\"v\":\"s\"}\n".to_owned(),
            "~1\t1\t2\n",
        ),
        (
            "copy:id",
            format!("COPY public.t (id, v) FROM stdin;\n1\t{long}\n2\ty\n\\.\n"),
            "COPY public.t (id, v) FROM stdin;\n2\ty\n1\tshort\n\\.\n".to_owned(),
            "~1\t2\t3\n",
        ),
    ];
    for (spec, a, b, changed) in cases {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = pair(dir.path(), &a, &b);
        // Only the last refusal asks for the record's window, so the one
        // rerun after it succeeds.
        let (refused, last) = follow(&["--key", spec], "64K".into(), &a, &b);
        let asked: Vec<u64> = refused
            .iter()
            .map(|mem| requested(&rift(&["--key", spec], mem, &a, &b)))
            .collect();
        let (record, small) = asked.split_last().unwrap();
        assert!(*record > 150_000, "{spec}: {refused:?} asked {asked:?}");
        assert!(small.iter().all(|&n| n < 1024), "{spec}: {asked:?}");
        assert_eq!(last.status.code(), Some(1), "{spec} from 64K");
        assert_eq!(String::from_utf8_lossy(&last.stdout), changed, "{spec}");
    }
}

/// The generator's pairs from 64K: each advice is at least twice the
/// `--mem` that was refused, so the chain is as long as the doublings to
/// the pair's need, not as long as the stages that each ask for a little.
#[test]
fn following_the_advice_converges_in_a_few_steps() {
    use rift_gen::{Config, Profile};
    for (profile, size, ratio) in [
        (Profile::Nokey, 4 << 20, 0.2),
        (Profile::Text, 8 << 20, 0.05),
    ] {
        let mut config = Config::new(5, size, profile);
        config.change_ratio = ratio;
        let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
        rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = pair(
            dir.path(),
            &String::from_utf8(a).unwrap(),
            &String::from_utf8(b).unwrap(),
        );
        let (refused, last) = follow(&[], "64K".into(), &a, &b);
        // 64K doubled 8 times is 16M, past the 10.5M these pairs need.
        assert!(refused.len() <= 8, "{profile:?}: {refused:?}");
        for w in refused.windows(2) {
            assert!(bytes(&w[1]) >= 2 * bytes(&w[0]), "{profile:?}: {refused:?}");
        }
        assert_eq!(last.status.code(), Some(1), "{profile:?}");
    }
}

/// Just above the full diff's smallest `--mem`, segments can come out
/// approximate for lack of budget. `--stats` then names a `--mem` to try,
/// and at that value there are none. Every budget of the band that the
/// move list's reserve opened (10300K to 11800K) on the two pairs that
/// showed it.
#[test]
#[ignore = "generates 16 and 4 MiB pairs and runs about 200 diffs; run in release"]
fn approx_for_lack_of_budget_names_a_mem_that_removes_it() {
    use rift_gen::{Config, Profile};
    let cases = [
        ("shuffled", Profile::Shuffled, 7, 16 << 20, 0.05),
        ("nokey", Profile::Nokey, 5, 4 << 20, 0.2),
    ];
    // Segments that ran out of budget; the other reasons do not follow
    // --mem, and the nokey pair keeps two of them at any budget.
    let approx = |out: &Output| -> u64 {
        String::from_utf8_lossy(&out.stderr)
            .lines()
            .filter(|l| l.contains("too large to align within --mem"))
            .map(|l| l.split_whitespace().next().unwrap().parse::<u64>().unwrap())
            .sum()
    };
    for (name, profile, seed, size, ratio) in cases {
        let mut config = Config::new(seed, size, profile);
        config.change_ratio = ratio;
        let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
        rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (a_path, b_path) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::write(&a_path, &a).unwrap();
        std::fs::write(&b_path, &b).unwrap();
        let mut seen = 0;
        for k in (10_300..=11_800).step_by(32) {
            let mem = format!("{k}K");
            let out = rift(&["--stats"], &mem, &a_path, &b_path);
            // Under the full diff's smallest --mem the run is refused.
            if out.status.code() == Some(2) || approx(&out) == 0 {
                continue;
            }
            seen += 1;
            let stderr = String::from_utf8_lossy(&out.stderr);
            let (_, rest) = stderr
                .split_once("; try --mem ")
                .unwrap_or_else(|| panic!("{name} {mem}: approx without a figure:\n{stderr}"));
            let next = rest.lines().next().unwrap();
            assert!(bytes(next) > bytes(&mem), "{name} {mem} names {next}");
            let again = rift(&["--stats"], next, &a_path, &b_path);
            assert_eq!(approx(&again), 0, "{name}: {mem} names {next}");
        }
        assert!(
            seen > 0,
            "{name}: no budget of the band came out approximate"
        );
    }
}
