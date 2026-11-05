#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    reason = "test inputs are small"
)]

use std::time::Instant;

use imara_diff::{Algorithm, Diff as Imara, Token};
use proptest::prelude::*;
use xxhash_rust::xxh3::xxh3_64;

use super::*;
use crate::budget::{Budget, Half};
use crate::lines::MemLines;
use crate::segment::SegmentStatus;
use crate::types::LineNo;

/// Lines of a file, terminators included.
fn lines(data: &[u8]) -> Vec<&[u8]> {
    let m = MemLines::new(data);
    (0..m.line_count()).map(|i| m.line(i).unwrap()).collect()
}

fn text(values: &[u8]) -> Vec<u8> {
    values
        .iter()
        .flat_map(|v| format!("{v}\n").into_bytes())
        .collect()
}

/// Replays `hunk` against `a` and checks the tiling invariant on the way.
fn apply(hunk: &Hunk, seg: &Segment, a: &[&[u8]], b: &[&[u8]]) -> Vec<Vec<u8>> {
    let (mut ia, mut ib) = (seg.a.start.0, seg.b.start.0);
    let mut out = Vec::new();
    for e in &hunk.edits {
        match *e {
            Edit::Equal { a: x, b: y, len } => {
                assert_eq!((x, y), (LineNo(ia), LineNo(ib)), "equal run out of order");
                for k in 0..len {
                    out.push(a[(ia + k) as usize].to_vec());
                }
                ia += len;
                ib += len;
            }
            Edit::Delete { a: x, len } => {
                assert_eq!(x, LineNo(ia), "delete run out of order");
                ia += len;
            }
            Edit::Insert { b: y, len } => {
                assert_eq!(y, LineNo(ib), "insert run out of order");
                for k in 0..len {
                    out.push(b[(ib + k) as usize].to_vec());
                }
                ib += len;
            }
        }
    }
    assert_eq!(
        (ia, ib),
        (seg.a.end.0, seg.b.end.0),
        "runs must cover the segment"
    );
    out
}

fn edit_count(hunk: &Hunk) -> u64 {
    hunk.edits
        .iter()
        .map(|e| match *e {
            Edit::Equal { .. } => 0,
            Edit::Delete { len, .. } | Edit::Insert { len, .. } => len,
        })
        .sum()
}

/// Refines `a[skip..]` against `b[skip..]` as one segment, checks the
/// script replays to B and returns it.
fn check(r: &mut Refiner, a: &[u8], b: &[u8], skip: u64, hash: fn(&[u8]) -> u64) -> Hunk {
    let (la, lb) = (lines(a), lines(b));
    let (ma, mb) = (MemLines::new(a), MemLines::new(b));
    let seg = Segment {
        a: LineNo(skip)..LineNo(la.len() as u64),
        b: LineNo(skip)..LineNo(lb.len() as u64),
        status: SegmentStatus::Differs,
    };
    let ha: Vec<_> = la[skip as usize..].iter().map(|l| hash(l)).collect();
    let hb: Vec<_> = lb[skip as usize..].iter().map(|l| hash(l)).collect();
    let budget = Budget::new(Bytes::mib(64));
    let mut res = budget
        .try_reserve(
            Half::Refine,
            work_estimate(ha.len() as u64, hb.len() as u64),
        )
        .unwrap();
    let hunk = r.refine(&seg, &ha, &hb, &ma, &mb, &mut res).unwrap();
    let replay = apply(&hunk, &seg, &la, &lb);
    let want: Vec<Vec<u8>> = lb[skip as usize..].iter().map(|l| l.to_vec()).collect();
    assert_eq!(replay, want, "script applied to A must yield B");
    hunk
}

/// Minimal edit count from a plain LCS table, the oracle for minimality.
fn lcs_edits(a: &[u8], b: &[u8]) -> u64 {
    let w = b.len() + 1;
    let mut t = vec![0u64; (a.len() + 1) * w];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            t[i * w + j] = if a[i] == b[j] {
                t[(i + 1) * w + j + 1] + 1
            } else {
                t[(i + 1) * w + j].max(t[i * w + j + 1])
            };
        }
    }
    (a.len() + b.len()) as u64 - 2 * t[0]
}

/// imara-diff 0.2.0 `MyersMinimal` edit count. It is not always minimal
/// (`[5, 1, 0, 1, 1]` against `[0, 0]` gives 7, the LCS gives 5), so it
/// serves as an upper bound, not as the oracle.
fn imara(a: &[u8], b: &[u8]) -> u64 {
    let tok = |v: &[u8]| v.iter().map(|&x| Token(u32::from(x))).collect::<Vec<_>>();
    let mut d = Imara::default();
    d.compute_with(Algorithm::MyersMinimal, &tok(a), &tok(b), 256);
    u64::from(d.count_additions() + d.count_removals())
}

/// A refiner that never cuts a gap short.
fn exact() -> Refiner {
    Refiner::new(u64::MAX)
}

fn values() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(0u8..6, 0..40)
}

/// `a` plus a few random edits, the shape of a real segment.
fn mutated() -> impl Strategy<Value = (Vec<u8>, Vec<u8>)> {
    (
        prop::collection::vec(0u8..20, 1..60),
        prop::collection::vec((any::<u8>(), 0u8..20), 0..6),
    )
        .prop_map(|(a, ops)| {
            let mut b = a.clone();
            for (pos, v) in ops {
                let i = pos as usize % (b.len() + 1);
                match v % 3 {
                    0 => b.insert(i, v),
                    1 if i < b.len() => {
                        b.remove(i);
                    }
                    _ if i < b.len() => b[i] = v,
                    _ => {}
                }
            }
            (a, b)
        })
}

#[test]
fn empty_sides_and_identical_input() {
    let mut r = exact();
    assert!(check(&mut r, b"", b"", 0, xxh3_64).edits.is_empty());
    assert_eq!(
        check(&mut r, b"", b"x\ny\n", 0, xxh3_64).edits,
        [Edit::Insert {
            b: LineNo(0),
            len: 2
        }]
    );
    assert_eq!(
        check(&mut r, b"x\ny\n", b"", 0, xxh3_64).edits,
        [Edit::Delete {
            a: LineNo(0),
            len: 2
        }]
    );
    assert_eq!(
        check(&mut r, b"x\ny", b"x\ny", 0, xxh3_64).edits,
        [Edit::Equal {
            a: LineNo(0),
            b: LineNo(0),
            len: 2
        }]
    );
}

#[test]
fn runs_use_absolute_line_numbers() {
    let a = text(&[9, 9, 1, 2, 3, 4]);
    let b = text(&[9, 9, 1, 7, 3, 4, 5]);
    let hunk = check(&mut exact(), &a, &b, 2, xxh3_64);
    assert_eq!(
        hunk.edits,
        [
            Edit::Equal {
                a: LineNo(2),
                b: LineNo(2),
                len: 1
            },
            Edit::Delete {
                a: LineNo(3),
                len: 1
            },
            Edit::Insert {
                b: LineNo(3),
                len: 1
            },
            Edit::Equal {
                a: LineNo(4),
                b: LineNo(4),
                len: 2
            },
            Edit::Insert {
                b: LineNo(6),
                len: 1
            },
        ]
    );
    assert!(!hunk.approx);
}

#[test]
fn verification_spans_several_read_blocks() {
    // 3 000 lines, 40 edits, colliding hashes: pairs cross the
    // VERIFY_LINES boundary and flips happen in every block.
    let mut rng = Rng(9);
    let a: Vec<u8> = (0..3_000).map(|_| rng.below(200) as u8).collect();
    let mut b = a.clone();
    for _ in 0..40 {
        let i = rng.below(b.len());
        match rng.below(3) {
            0 => b.insert(i, rng.below(200) as u8),
            1 => {
                b.remove(i);
            }
            _ => b[i] = rng.below(200) as u8,
        }
    }
    for cutoff in [1, u64::MAX] {
        let mut r = Refiner::new(cutoff);
        check(&mut r, &text(&a), &text(&b), 5, |l| u64::from(l[0] % 7));
        r.patience = false;
        check(&mut r, &text(&a), &text(&b), 5, xxh3_64);
    }
}

#[test]
fn colliding_hashes_still_give_a_correct_script() {
    let all_same = |_: &[u8]| 7u64;
    let parity = |l: &[u8]| u64::from(l[0] & 1);
    let mut r = exact();
    for hash in [all_same as fn(&[u8]) -> u64, parity] {
        let a = text(&[1, 2, 3, 4, 5, 6, 7, 8]);
        let b = text(&[1, 2, 9, 4, 5, 6, 8, 10]);
        let hunk = check(&mut r, &a, &b, 0, hash);
        assert!(edit_count(&hunk) >= 4, "every true difference is reported");
        check(&mut r, &text(&[1, 1, 1, 2]), &text(&[2, 1, 1, 1]), 0, hash);
        check(&mut r, &text(&[3, 3, 3]), &text(&[3, 3, 3]), 0, hash);
    }
    // Patience off: DP and Myers see one big hash-equal block.
    r.patience = false;
    for hash in [all_same as fn(&[u8]) -> u64, parity] {
        let a: Vec<u8> = (0..200).map(|i| (i * 7 % 13) as u8).collect();
        let b: Vec<u8> = (0..190).map(|i| (i * 5 % 11) as u8).collect();
        check(&mut r, &text(&a), &text(&b), 0, hash);
    }
}

#[test]
fn cutoff_marks_approx_and_keeps_the_script_valid() {
    // No unique lines, D large: Myers is forced and cut short.
    let mut rng = Rng(5);
    let a: Vec<u8> = (0..300).map(|_| rng.below(3) as u8).collect();
    let b: Vec<u8> = (0..300).map(|_| rng.below(3) as u8).collect();
    let mut r = Refiner::new(1);
    let hunk = check(&mut r, &text(&a), &text(&b), 0, xxh3_64);
    assert!(hunk.approx);
    let exact_hunk = check(&mut exact(), &text(&a), &text(&b), 0, xxh3_64);
    assert!(!exact_hunk.approx);
    assert_eq!(edit_count(&exact_hunk), lcs_edits(&a, &b));
    assert!(edit_count(&hunk) >= edit_count(&exact_hunk));
}

#[test]
fn small_reservation_grows_or_fails_with_budget_error() {
    let a = text(&[1, 2, 3]);
    let m = MemLines::new(&a);
    let seg = Segment {
        a: LineNo(0)..LineNo(3),
        b: LineNo(0)..LineNo(3),
        status: SegmentStatus::Differs,
    };
    let h = [1, 2, 3];
    let estimate = work_estimate(3, 3);
    let budget = Budget::new(Bytes::mib(1));
    let mut res = budget.try_reserve(Half::Refine, Bytes(10)).unwrap();
    exact().refine(&seg, &h, &h, &m, &m, &mut res).unwrap();
    assert_eq!(res.bytes(), estimate, "grown to the estimate");
    let budget = Budget::new(Bytes(2 * estimate.0 - 2));
    let mut res = budget.try_reserve(Half::Refine, Bytes(10)).unwrap();
    let err = exact().refine(&seg, &h, &h, &m, &m, &mut res).unwrap_err();
    assert!(matches!(err, Error::Budget(_)), "{err}");
    assert_eq!(
        res.bytes(),
        Bytes(10),
        "failed growth leaves the reservation"
    );
}

#[test]
fn estimate_grows_with_both_sides() {
    assert!(work_estimate(1000, 0) > work_estimate(0, 0));
    assert!(work_estimate(1000, 1000).0 > work_estimate(1000, 0).0 + work_estimate(0, 1000).0);
    assert_eq!(work_estimate(10, 20), work_estimate(20, 10));
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10_000))]

    #[test]
    fn script_applied_to_a_yields_b(a in values(), b in values()) {
        check(&mut exact(), &text(&a), &text(&b), 0, xxh3_64);
    }

    #[test]
    fn script_for_mutated_input_yields_b((a, b) in mutated(), cutoff in 0u64..4) {
        let hunk = check(&mut Refiner::new(cutoff), &text(&a), &text(&b), 0, xxh3_64);
        prop_assert!(edit_count(&hunk) <= (a.len() + b.len()) as u64);
    }

    #[test]
    fn myers_and_dp_match_imara_minimal(a in values(), b in values(), dp in 0usize..=DP_MAX) {
        let mut r = exact();
        r.patience = false;
        r.dp_max = dp;
        let hunk = check(&mut r, &text(&a), &text(&b), 0, xxh3_64);
        prop_assert!(!hunk.approx);
        let ours = edit_count(&hunk);
        prop_assert_eq!(ours, lcs_edits(&a, &b));
        prop_assert!(ours <= imara(&a, &b));
    }

    #[test]
    fn colliding_hashes_on_random_input((a, b) in mutated()) {
        check(&mut exact(), &text(&a), &text(&b), 0, |l| u64::from(l[0] % 3));
    }
}

/// xorshift64*, enough for test data.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Patience is not minimal, so the whole pipeline is compared in aggregate
/// over 4 000 random mutated inputs against the LCS oracle and imara-diff:
/// the exact script has at most 1.1 times the minimal edits, and a cut
/// short script at most the factor listed per cutoff. Measured on seed 42:
/// exact 1.07, cutoff 1 (most searches cut) 2.9, cutoff 2 1.23, cutoff 4 1.08.
#[test]
fn pipeline_edit_counts_stay_near_imara() {
    const CUTOFFS: [(u64, u64); 3] = [(1, 35), (2, 15), (4, 12)];
    let mut rng = Rng(42);
    let (mut ours, mut theirs, mut reference) = (0, 0, 0);
    let mut approx = [0u64; 3];
    for _ in 0..4_000 {
        let n = 1 + rng.below(80);
        let a: Vec<u8> = (0..n).map(|_| rng.below(12) as u8).collect();
        let mut b = a.clone();
        for _ in 0..rng.below(8) {
            let i = rng.below(b.len() + 1);
            match rng.below(3) {
                0 => b.insert(i, rng.below(12) as u8),
                1 if i < b.len() => {
                    b.remove(i);
                }
                _ if i < b.len() => b[i] = rng.below(12) as u8,
                _ => {}
            }
        }
        let (ta, tb) = (text(&a), text(&b));
        ours += edit_count(&check(&mut exact(), &ta, &tb, 0, xxh3_64));
        for (k, (cutoff, _)) in CUTOFFS.iter().enumerate() {
            approx[k] += edit_count(&check(&mut Refiner::new(*cutoff), &ta, &tb, 0, xxh3_64));
        }
        theirs += lcs_edits(&a, &b);
        reference += imara(&a, &b);
    }
    eprintln!("edits: exact {ours}, cutoffs 1/2/4 {approx:?}, minimal {theirs}, imara {reference}");
    assert!(ours * 10 <= theirs * 11, "exact {ours} vs minimal {theirs}");
    assert!(
        ours * 10 <= reference * 11,
        "exact {ours} vs imara {reference}"
    );
    for (k, (cutoff, tenths)) in CUTOFFS.iter().enumerate() {
        assert!(
            approx[k] * 10 <= theirs * tenths,
            "cutoff {cutoff}: {} vs minimal {theirs}",
            approx[k]
        );
    }
}

/// `n` lines of A and B with `rate` changed lines, as `spike mutate` does.
fn big_pair(n: usize, rate: f64, seed: u64) -> (Vec<u8>, Vec<u8>) {
    let mut rng = Rng(seed);
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let mut op = 0;
    let every = (1.0 / rate) as usize;
    for i in 0..n {
        let line = format!("{i}\t{:016x}\tvalue\n", rng.next());
        a.extend_from_slice(line.as_bytes());
        if rng.below(every) == 0 {
            op += 1;
            match op % 3 {
                0 => b.extend_from_slice(format!("{i}\tchanged\n").as_bytes()),
                1 => {}
                _ => {
                    b.extend_from_slice(b"inserted\n");
                    b.extend_from_slice(line.as_bytes());
                }
            }
        } else {
            b.extend_from_slice(line.as_bytes());
        }
    }
    (a, b)
}

fn time_refine(r: &mut Refiner, a: &[u8], b: &[u8]) -> (f64, Hunk) {
    let (ma, mb) = (MemLines::new(a), MemLines::new(b));
    let ha: Vec<u64> = lines(a).iter().map(|l| xxh3_64(l)).collect();
    let hb: Vec<u64> = lines(b).iter().map(|l| xxh3_64(l)).collect();
    let seg = Segment {
        a: LineNo(0)..LineNo(ha.len() as u64),
        b: LineNo(0)..LineNo(hb.len() as u64),
        status: SegmentStatus::Differs,
    };
    let est = work_estimate(ha.len() as u64, hb.len() as u64);
    let budget = Budget::new(Bytes::mib(256));
    let mut res = budget.try_reserve(Half::Refine, est).unwrap();
    let t = Instant::now();
    let hunk = r.refine(&seg, &ha, &hb, &ma, &mb, &mut res).unwrap();
    (t.elapsed().as_secs_f64(), hunk)
}

/// Run with `cargo test -p rift-core --release -- --ignored throughput --nocapture`.
#[test]
#[ignore = "benchmark: 1M-line segment with 0.1% changes"]
fn throughput_1m_lines() {
    let (a, b) = big_pair(1_000_000, 0.001, 7);
    let mut r = Refiner::new(1000);
    let (warm, _) = time_refine(&mut r, &a, &b);
    let mut best = warm;
    for _ in 0..4 {
        let (t, hunk) = time_refine(&mut r, &a, &b);
        assert!(!hunk.approx);
        best = best.min(t);
    }
    let (la, lb) = (lines(&a).len(), lines(&b).len());
    eprintln!(
        "1M lines, 0.1% changes: a={la} b={lb} lines, best {best:.3} s, {:.2} M lines/s (A+B), estimate {} MiB",
        (la + lb) as f64 / best / 1e6,
        work_estimate(la as u64, lb as u64).0 >> 20
    );
}

/// Run with `cargo test -p rift-core --release -- --ignored dp_threshold --nocapture`.
#[test]
#[ignore = "benchmark: DP threshold sweep"]
fn dp_threshold() {
    let mut rng = Rng(3);
    let mut gaps = Vec::new();
    for _ in 0..20_000 {
        let n = 2 + rng.below(100);
        let a: Vec<u8> = (0..n).map(|_| rng.below(8) as u8).collect();
        let mut b = a.clone();
        for _ in 0..=rng.below(6) {
            if b.is_empty() {
                break;
            }
            let i = rng.below(b.len());
            match rng.below(3) {
                0 => b.insert(i, rng.below(8) as u8),
                1 => {
                    b.remove(i);
                }
                _ => b[i] = rng.below(8) as u8,
            }
        }
        gaps.push((text(&a), text(&b)));
    }
    let (big_a, big_b) = big_pair(1_000_000, 0.001, 7);
    // Raise DP_MAX temporarily to sweep above it: the table is sized by it.
    for dp in [0, 4, 8, 12, 16, 24, 32, 64]
        .into_iter()
        .filter(|&d| d <= DP_MAX)
    {
        let mut r = exact();
        r.patience = false;
        r.dp_max = dp;
        let mut total = f64::MAX;
        for _ in 0..3 {
            let mut run = 0.0;
            for (a, b) in &gaps {
                run += time_refine(&mut r, a, b).0;
            }
            total = total.min(run);
        }
        let mut r = Refiner::new(1000);
        r.dp_max = dp;
        let big = (0..3)
            .map(|_| time_refine(&mut r, &big_a, &big_b).0)
            .fold(f64::MAX, f64::min);
        eprintln!("dp_max {dp:3}: 20k random gaps {total:.3} s, 1M-line segment {big:.3} s");
    }
}

#[test]
fn lines_longer_than_the_reader_are_compared_in_pieces() {
    use crate::index::IndexedLines;
    use crate::scan::{ScanConfig, scan};

    let long = |tail: u8| {
        let mut l = vec![b'x'; 200_000];
        l.push(tail);
        l.push(b'\n');
        l
    };
    let build = |parts: &[&[u8]]| parts.concat();
    let a = build(&[b"a\n", &long(b'1'), b"b\n", &long(b'2'), b"z\n"]);
    let b = build(&[b"a\n", &long(b'1'), b"c\n", &long(b'3'), b"z\n"]);
    let budget = Budget::new(Bytes::mib(64));
    let reader = |data: &[u8]| {
        let cfg = ScanConfig::for_pair(Bytes(data.len() as u64), Bytes(0), &budget).0;
        scan(data, &budget, &cfg).unwrap()
    };
    let (sa, sb) = (reader(&a), reader(&b));
    let la = IndexedLines::new(&a[..], sa.index(), &budget, Half::Refine, Bytes(4096)).unwrap();
    let lb = IndexedLines::new(&b[..], sb.index(), &budget, Half::Refine, Bytes(4096)).unwrap();
    let seg = Segment {
        a: LineNo(0)..LineNo(5),
        b: LineNo(0)..LineNo(5),
        status: SegmentStatus::Differs,
    };
    let hashes = |d: &[u8]| lines(d).into_iter().map(xxh3_64).collect::<Vec<_>>();
    let (ha, hb) = (hashes(&a), hashes(&b));
    let mut res = budget.try_reserve(Half::Refine, Bytes(0)).unwrap();
    let hunk = Refiner::new(256)
        .refine(&seg, &ha, &hb, &la, &lb, &mut res)
        .unwrap();
    assert_eq!(
        apply(&hunk, &seg, &lines(&a), &lines(&b)).concat(),
        b,
        "script replays to B"
    );
    assert_eq!(edit_count(&hunk), 4, "the equal long line stays equal");

    // A hash collision on the long lines: verification must catch it.
    let mut hb2 = hb.clone();
    hb2[3] = ha[3];
    let hunk = Refiner::new(256)
        .refine(&seg, &ha, &hb2, &la, &lb, &mut res)
        .unwrap();
    assert_eq!(apply(&hunk, &seg, &lines(&a), &lines(&b)).concat(), b);
}
