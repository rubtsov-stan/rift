#![allow(clippy::cast_possible_truncation, reason = "test inputs are small")]

use std::sync::Arc;

use proptest::prelude::*;

use super::*;
use crate::edit::ApproxReason;
use crate::lines::MemLines;
use crate::scan::{ScanConfig, scan};
use crate::script::{apply, validate};
use crate::unified::Label;

fn opts() -> Options<'static> {
    let label = |name: &'static str| Label {
        name: name.as_bytes(),
        mtime: None,
    };
    Options {
        context: 3,
        old: label("a"),
        new: label("b"),
    }
}

/// Every hunk of a diff, waited for in order.
fn script<A: Source + Send + ?Sized, B: Source + Send + ?Sized>(d: &Diff<A, B>) -> Vec<Hunk> {
    (0..d.segments().len())
        .map(|i| (*d.sched.wait_hunk(i).unwrap()).clone())
        .collect()
}

/// Checks that the diff's script is valid and replays A into B, and
/// returns it.
fn check(a: &[u8], b: &[u8], mem: Bytes, threads: usize) -> Vec<Hunk> {
    let d = Diff::new(
        Arc::<[u8]>::from(a),
        Arc::<[u8]>::from(b),
        &Budget::new(mem),
        threads,
    )
    .unwrap();
    let hunks = script(&d);
    let (ma, mb) = (MemLines::new(a), MemLines::new(b));
    validate(&hunks, ma.line_count(), mb.line_count()).unwrap();
    let mut out = Vec::new();
    apply(&ma, &mb, &hunks, &mut out).unwrap();
    assert_eq!(out, b, "script replays A into B");
    hunks
}

fn rows(n: u64, f: impl Fn(u64) -> String) -> Vec<u8> {
    (0..n).flat_map(|i| f(i).into_bytes()).collect()
}

#[test]
fn a_generated_pair_replays_and_writes_like_its_script() {
    let a = rows(20_000, |i| format!("row {i} {}\n", i % 13));
    let b = rows(20_000, |i| match i % 997 {
        5 => String::new(),
        6 => format!("changed {i}\nadded {i}\n"),
        _ => format!("row {i} {}\n", i % 13),
    });
    let hunks = check(&a, &b, Bytes::mib(64), 3);
    assert!(hunks.iter().all(|h| h.approx.is_none()));
    let d = Diff::new(
        Arc::<[u8]>::from(&a[..]),
        Arc::<[u8]>::from(&b[..]),
        &Budget::new(Bytes::mib(64)),
        2,
    )
    .unwrap();
    let mut via_view = Vec::new();
    let written = d.write_unified(&opts(), &mut via_view).unwrap();
    assert_eq!(written.verdict, Verdict::Different);
    let mut via_script = Vec::new();
    write_diff(
        &MemLines::new(&a),
        &MemLines::new(&b),
        |i| Ok(hunks.get(i)),
        &opts(),
        &mut via_script,
    )
    .unwrap();
    assert_eq!(via_view, via_script);
}

#[test]
fn empty_and_identical_inputs() {
    for (a, b) in [
        (&b""[..], &b""[..]),
        (b"", b"x\n"),
        (b"x\n", b""),
        (b"same\nlines\n", b"same\nlines\n"),
        (b"no newline", b"no newline"),
    ] {
        check(a, b, Bytes::mib(16), 1);
        let d = Diff::new(
            Arc::<[u8]>::from(a),
            Arc::<[u8]>::from(b),
            &Budget::new(Bytes::mib(16)),
            1,
        )
        .unwrap();
        let mut out = Vec::new();
        let w = d.write_unified(&opts(), &mut out).unwrap();
        assert_eq!(w.verdict == Verdict::Identical, a == b);
        assert_eq!(out.is_empty(), a == b);
    }
}

/// A worker over two in-memory files, for calling the large-segment path
/// directly with a chosen limit.
fn worker(a: &[u8], b: &[u8]) -> (Worker<[u8], [u8]>, Budget) {
    let budget = Budget::new(Bytes::mib(256));
    let index = |d: &[u8]| {
        let cfg = ScanConfig::for_pair(Bytes(d.len() as u64), Bytes(0), &budget).0;
        Arc::new(scan(d, &budget, &cfg).unwrap().into_index().0)
    };
    let reader = |d: &[u8]| {
        IndexedLines::new(
            Arc::<[u8]>::from(d),
            index(d),
            &budget,
            Half::Refine,
            WORKER_SCRATCH,
        )
        .unwrap()
    };
    let w = Worker {
        refiner: Refiner::new(CUTOFF),
        a: reader(a),
        b: reader(b),
        buf: LineBuf::new(),
        buf2: LineBuf::new(),
    };
    (w, budget)
}

/// Runs the large-segment path on the whole pair under `limit` and checks
/// that the hunk replays A into B and fits the limit.
fn large(a: &[u8], b: &[u8], limit: Bytes) -> Hunk {
    let (mut w, _budget) = worker(a, b);
    let (ma, mb) = (MemLines::new(a), MemLines::new(b));
    let seg = Segment {
        a: LineNo(0)..LineNo(ma.line_count()),
        b: LineNo(0)..LineNo(mb.line_count()),
        status: SegmentStatus::Differs,
    };
    let hunk = resample::refine_large(&mut w, &seg, limit, &Cancel::default()).unwrap();
    let mut out = Vec::new();
    apply(&ma, &mb, [&hunk], &mut out).unwrap();
    assert_eq!(out, b, "script replays A into B");
    let bytes = HUNK_OVERHEAD.0 + hunk.edits.capacity() as u64 * EDIT_BYTES;
    assert!(bytes <= limit.0, "hunk of {bytes} bytes over {limit}");
    hunk
}

fn changes(h: &Hunk) -> u64 {
    h.edits
        .iter()
        .map(|e| match *e {
            Edit::Equal { .. } => 0,
            Edit::Delete { len, .. } | Edit::Insert { len, .. } => len,
        })
        .sum()
}

#[test]
fn a_segment_too_large_for_a_job_is_resampled_not_approximated() {
    let a = rows(30_000, |i| format!("unique line {i}\n"));
    let b = rows(30_000, |i| {
        if i % 1000 == 7 {
            format!("edited line {i}\n")
        } else {
            format!("unique line {i}\n")
        }
    });
    // About 60 bytes per line: the whole segment would need 3.6 MB.
    assert!(estimate(30_000, 30_000) > Bytes::mib(3));
    let hunk = large(&a, &b, Bytes(512 << 10));
    assert!(hunk.approx.is_none());
    assert_eq!(changes(&hunk), 60, "30 lines changed on each side");
}

#[test]
fn a_large_range_without_unique_lines_is_approximate_but_valid() {
    let a = b"x\n".repeat(40_000);
    let mut b = a.clone();
    b[2 * 20_000] = b'y';
    // With room to look the hashes up, the sides share them.
    let hunk = large(&a, &b, Bytes(1 << 20));
    assert_eq!(hunk.approx, Some(ApproxReason::NoAnchor));
    // Without it nothing is known, and budget is the reason.
    let hunk = large(&a, &b, Bytes(256 << 10));
    assert_eq!(hunk.approx, Some(ApproxReason::Budget));
}

#[test]
fn a_large_range_without_a_common_line_is_replaced_exactly() {
    let a = rows(20_000, |i| format!("a {}\n", i % 7));
    let b = rows(20_000, |i| format!("b {i}\n"));
    // The check holds the hashes of one side, 160 kB of the 256 kB
    // skeleton half a 512 KiB job has.
    let hunk = large(&a, &b, Bytes(512 << 10));
    assert!(hunk.approx.is_none());
    assert_eq!(changes(&hunk), 40_000);
    // Without room for them nothing is proved, and the hunk stays
    // approximate for lack of budget.
    let hunk = large(&a, &b, Bytes(256 << 10));
    assert_eq!(hunk.approx, Some(ApproxReason::Budget));
}

#[test]
fn a_pair_without_a_common_line_is_exact_at_any_budget() {
    let a = rows(60_000, |i| format!("left {}\n", i % 5));
    let b = rows(50_000, |i| format!("right {i}\n"));
    // 16 MiB sends the segment through resampling, 256 MiB refines it whole.
    for mem in [Bytes::mib(16), Bytes::mib(256)] {
        let hunks = check(&a, &b, mem, 2);
        assert!(hunks.iter().all(|h| h.approx.is_none()), "{mem}");
    }
}

#[test]
fn a_large_equal_range_is_compared_not_replaced() {
    let a = b"x\n".repeat(40_000);
    let hunk = large(&a, &a, Bytes(256 << 10));
    assert!(hunk.approx.is_none());
    assert_eq!(changes(&hunk), 0);
}

#[test]
fn too_many_edits_close_the_rest_as_one_replacement() {
    // Every other line changes: far more edits than 64 KiB can hold.
    let a = rows(30_000, |i| format!("line {i}\n"));
    let b = rows(30_000, |i| {
        if i % 2 == 0 {
            format!("line {i}\n")
        } else {
            format!("other {i}\n")
        }
    });
    let hunk = large(&a, &b, Bytes(64 << 10));
    assert_eq!(hunk.approx, Some(ApproxReason::Budget));
}

#[test]
fn long_lines_in_a_large_segment() {
    let long = |c: u8| {
        let mut l = vec![c; 300_000];
        l.push(b'\n');
        l
    };
    let mut a = rows(5_000, |i| format!("l {i}\n"));
    let mut b = a.clone();
    a.extend_from_slice(&long(b'p'));
    b.extend_from_slice(&long(b'q'));
    a.extend_from_slice(&rows(5_000, |i| format!("m {i}\n")));
    b.extend_from_slice(&rows(5_000, |i| format!("m {i}\n")));
    let hunk = large(&a, &b, Bytes(128 << 10));
    assert_eq!(changes(&hunk), 2);
    check(&a, &b, Bytes::mib(16), 2);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Tiny limits send random pairs through resampling, nested levels,
    /// the equality shortcut and the full edit list.
    #[test]
    fn the_large_segment_path_replays_random_pairs(
        a in prop::collection::vec(0u8..8, 0..600),
        b in prop::collection::vec(0u8..8, 0..600),
        limit in prop::sample::select(vec![2u64 << 10, 6 << 10, 16 << 10, 64 << 10]),
    ) {
        let text = |v: &[u8]| -> Vec<u8> {
            v.iter().enumerate().flat_map(|(i, &x)| {
                if x < 3 { format!("u{}\n", i / 2).into_bytes() } else { format!("{x}\n").into_bytes() }
            }).collect()
        };
        large(&text(&a), &text(&b), Bytes(limit));
    }

    /// Whole diffs at a few budgets.
    #[test]
    fn random_pairs_replay_under_any_budget(
        a in prop::collection::vec(0u8..6, 0..400),
        b in prop::collection::vec(0u8..6, 0..400),
        mem in prop::sample::select(vec![16u64 << 20, 24 << 20, 64 << 20]),
    ) {
        let text = |v: &[u8]| -> Vec<u8> {
            v.iter().enumerate().flat_map(|(i, &x)| {
                if x == 0 { format!("u{i}\n").into_bytes() } else { format!("{x}\n").into_bytes() }
            }).collect()
        };
        check(&text(&a), &text(&b), Bytes(mem), 2);
    }
}

#[test]
fn moves_off_finds_nothing_and_keeps_the_script() {
    let lines = |order: &[u64]| {
        rows(order.len() as u64, |i| {
            format!("line {}\n", order[i as usize])
        })
    };
    let a = lines(&(0..200).collect::<Vec<_>>());
    let b = lines(&(100..200).chain(0..100).collect::<Vec<_>>());
    let build = |moves| {
        Diff::with_config(
            Arc::<[u8]>::from(&a[..]),
            Arc::<[u8]>::from(&b[..]),
            &Budget::new(Bytes::mib(64)),
            2,
            Config { moves },
        )
        .unwrap()
    };
    let (on, off) = (build(true), build(false));
    assert!(!on.moves().blocks().is_empty() && !on.moves_disabled());
    assert!(off.moves().blocks().is_empty() && off.moves_disabled());
    assert_eq!(script(&on), script(&off));
}

#[test]
fn a_refine_half_too_small_for_one_worker_is_refused_whole() {
    let a = rows(2000, |i| format!("row {i}\n"));
    let b = rows(2000, |i| {
        if i % 100 == 0 {
            format!("changed {i}\n")
        } else {
            format!("row {i}\n")
        }
    });
    let budget = Budget::new(Bytes::mib(6));
    let built = Diff::new(
        Arc::<[u8]>::from(&a[..]),
        Arc::<[u8]>::from(&b[..]),
        &budget,
        2,
    );
    let Err(Error::Budget(e)) = built else {
        panic!("6 MiB leaves too little for one worker");
    };
    assert_eq!(e.half, Half::Refine);
    // Twice what the half held plus the request is enough to start.
    let held = budget.capacity(Half::Refine).0 - e.available.0;
    check(&a, &b, Bytes(2 * (held + e.requested.0)), 2);
}
