//! Moved blocks against the generator's manifest, and how the command line
//! reports them.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use rift_core::diff::Diff;
use rift_core::moves::{MIN_LINES, MovedBlock};
use rift_core::{Budget, Bytes, DiffView, LineNo};
use rift_gen::{Config, Kind, Mutation, Profile, Rows};

struct Pair {
    a: Vec<u8>,
    b: Vec<u8>,
    moves: Vec<Mutation>,
}

fn generate(mut config: Config) -> Pair {
    config.change_ratio = 0.003;
    let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
    rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
    let moves = rift_gen::parse_manifest(&m)
        .unwrap()
        .into_iter()
        .filter(|m| m.kind == Kind::Move)
        .collect();
    Pair { a, b, moves }
}

fn shuffled(rows: Rows, seed: u64) -> Pair {
    shuffled_of(2 << 20, rows, seed)
}

fn shuffled_of(size: u64, rows: Rows, seed: u64) -> Pair {
    let mut config = Config::new(seed, size, Profile::Shuffled);
    config.rows = rows;
    generate(config)
}

fn blocks(p: &Pair) -> Vec<MovedBlock> {
    let d = Diff::new(
        Arc::<[u8]>::from(&p.a[..]),
        Arc::<[u8]>::from(&p.b[..]),
        &Budget::new(Bytes(64 << 20)),
        2,
    )
    .unwrap();
    assert!(!d.moves().skipped());
    assert_eq!(d.moved_blocks(), d.moves().blocks());
    d.moved_blocks().to_vec()
}

fn block_of(m: &Mutation) -> MovedBlock {
    MovedBlock {
        a: LineNo(m.line_a)..LineNo(m.line_a + m.count),
        b: LineNo(m.line_b)..LineNo(m.line_b + m.count),
    }
}

/// Every manifest move of at least `MIN_LINES` lines is reported, and
/// nothing else, except that a block moved past fewer lines than its own
/// length (the generator does that at the end of a file) is reported the
/// other way round: as the lines it jumped over, the shorter side.
fn check(p: &Pair) {
    let found = blocks(p);
    for m in p.moves.iter().filter(|m| m.count >= MIN_LINES) {
        let swapped = |f: &MovedBlock| f.a.start.0 == m.line_a + m.count && f.b.end.0 == m.line_b;
        assert!(
            found.contains(&block_of(m)) || found.iter().any(swapped),
            "{m:?} not found"
        );
    }
    for f in &found {
        let real = p.moves.iter().any(|m| {
            block_of(m) == *f || (f.a.start.0 == m.line_a + m.count && f.b.end.0 == m.line_b)
        });
        assert!(real, "{f:?} is not a move of the manifest");
        assert!(f.a.end - f.a.start >= MIN_LINES);
    }
    assert!(found.windows(2).all(|w| w[0].a.end <= w[1].a.start));
}

#[test]
fn every_shuffled_block_move_is_found() {
    for rows in [Rows::Csv, Rows::Sql, Rows::Ndjson] {
        for seed in [1, 7] {
            let p = shuffled(rows, seed);
            assert!(p.moves.len() > 20, "{rows:?} has few moves");
            check(&p);
        }
    }
}

#[test]
fn moves_in_the_other_profiles_are_found() {
    for (seed, profile) in [
        (1, Profile::Sql),
        (2, Profile::Csv),
        (3, Profile::Ndjson),
        (4, Profile::Text),
    ] {
        let p = generate(Config::new(seed, 2 << 20, profile));
        assert!(!p.moves.is_empty(), "{profile:?} has no moves");
        check(&p);
    }
}

#[test]
fn pairs_without_moves_report_none() {
    let kinds = [
        Kind::Insert,
        Kind::Delete,
        Kind::Modify,
        Kind::Dup,
        Kind::LowUniq,
        Kind::Crlf,
        Kind::NoEol,
    ];
    for (seed, profile) in [
        (1, Profile::Sql),
        (2, Profile::Csv),
        (3, Profile::Ndjson),
        (4, Profile::Text),
        (5, Profile::Nokey),
    ] {
        let mut config = Config::new(seed, 2 << 20, profile);
        config.kinds = kinds.to_vec();
        let p = generate(config);
        assert!(blocks(&p).is_empty(), "{profile:?}");
    }
}

fn files(p: &Pair) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    std::fs::write(&a, &p.a).unwrap();
    std::fs::write(&b, &p.b).unwrap();
    (dir, a, b)
}

fn rift(args: &[&str], a: &Path, b: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_rift"))
        .args(args)
        .arg(a)
        .arg(b)
        .output()
        .unwrap()
}

/// The text both modes print for the first block.
fn first_line(p: &Pair) -> String {
    let f = &blocks(p)[0];
    format!(
        "  a {}..{}  b {}..{}\n",
        f.a.start, f.a.end, f.b.start, f.b.end
    )
}

#[test]
fn summary_and_stats_list_the_blocks() {
    let p = shuffled(Rows::Csv, 7);
    let n = blocks(&p).len();
    let (_dir, a, b) = files(&p);
    let head = format!("moved    {n} blocks, ");

    let out = rift(&["--summary"], &a, &b);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains(&head), "{text}");
    assert!(text.contains(&first_line(&p)), "{text}");
    assert!(
        text.contains(&format!("  ... and {} more\n", n - 10)),
        "{text}"
    );

    let out = rift(&["--stats"], &a, &b);
    let text = String::from_utf8(out.stderr).unwrap();
    assert!(text.contains(&head), "{text}");
    assert!(text.contains(&first_line(&p)), "{text}");
    let plain = rift(&[], &a, &b);
    assert_eq!(out.stdout, plain.stdout, "--stats changes only stderr");
}

#[test]
fn summary_says_when_the_budget_skips_moves() {
    // Small enough for the skeleton half of 256 KiB; the refine half has
    // no room for the comparison buffers next to the two readers.
    let p = shuffled_of(64 << 10, Rows::Csv, 7);
    let (_dir, a, b) = files(&p);
    let out = rift(&["--summary", "--mem", "512K"], &a, &b);
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(1), "{text}");
    assert!(
        text.contains("moved    not searched, the budget is too small\n"),
        "{text}"
    );
}

/// The search for moved blocks only annotates: with it off the unified
/// diff is byte for byte the same, at a roomy and a tight heap limit.
#[test]
fn no_moves_leaves_the_unified_diff_unchanged() {
    use std::fs;
    use std::process::Command;
    let cases = [
        (1, Profile::Shuffled, Rows::Csv),
        (2, Profile::Shuffled, Rows::Ndjson),
        (3, Profile::Sql, Rows::Csv),
        (4, Profile::Text, Rows::Csv),
        (5, Profile::Nokey, Rows::Csv),
    ];
    let dir = tempfile::tempdir().unwrap();
    let (pa, pb) = (dir.path().join("a"), dir.path().join("b"));
    for (seed, profile, rows) in cases {
        let mut config = Config::new(seed, 2 << 20, profile);
        config.rows = rows;
        let p = generate(config);
        fs::write(&pa, &p.a).unwrap();
        fs::write(&pb, &p.b).unwrap();
        for mem in ["16M", "256M"] {
            let run = |extra: &[&str]| {
                Command::new(env!("CARGO_BIN_EXE_rift"))
                    .args(["--mem", mem])
                    .args(extra)
                    .arg(&pa)
                    .arg(&pb)
                    .output()
                    .unwrap()
            };
            let (on, off) = (run(&[]), run(&["--no-moves"]));
            assert_eq!(on.status.code(), Some(1), "seed {seed} mem {mem}");
            assert_eq!(off.status.code(), Some(1), "seed {seed} mem {mem}");
            assert!(on.stdout == off.stdout, "seed {seed} mem {mem}");
        }
    }
}

#[test]
fn no_moves_is_reported_where_moves_are() {
    use std::fs;
    use std::process::Command;
    let p = shuffled(Rows::Csv, 6);
    let dir = tempfile::tempdir().unwrap();
    let (pa, pb) = (dir.path().join("a"), dir.path().join("b"));
    fs::write(&pa, &p.a).unwrap();
    fs::write(&pb, &p.b).unwrap();
    let rift = |extra: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_rift"))
            .args(extra)
            .arg(&pa)
            .arg(&pb)
            .output()
            .unwrap();
        (out.stdout, String::from_utf8(out.stderr).unwrap())
    };
    let (out, _) = rift(&["--summary", "--no-moves"]);
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("moved    not searched, --no-moves"), "{out}");
    let (_, err) = rift(&["--stats", "--no-moves"]);
    assert!(err.contains("moved    not searched, --no-moves"), "{err}");
    let (_, err) = rift(&["--key", "csv:1", "--no-moves"]);
    assert!(err.contains("cannot be used with"), "{err}");
}
