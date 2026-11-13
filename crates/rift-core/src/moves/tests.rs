use super::*;
use crate::budget::Half;
use crate::lines::MemLines;
use crate::segment::SegmentStatus;
use crate::skeleton::build_pair;
use crate::types::Bytes;

/// What the readers are said to return whole; `MemLines` has no limit.
const CAP: Bytes = Bytes(4 << 10);

fn rows(n: u64, f: impl Fn(u64) -> String) -> Vec<u8> {
    (0..n).flat_map(|i| f(i).into_bytes()).collect()
}

/// [`Moves::find`] with the block list reserved as `Diff` reserves it.
fn search(
    pairs: &[[LineNo; 2]],
    segments: &[Segment],
    a: &impl LineAccess,
    b: &impl LineAccess,
    budget: &Budget,
) -> Moves {
    let list = budget
        .try_reserve(Half::Refine, Moves::list_bytes(pairs.len(), budget))
        .ok();
    Moves::find(pairs, segments, a, b, CAP, budget, list).unwrap()
}

/// Moved blocks of `a` against `b`, found from the skeleton as `Diff` does.
fn moves(a: &[u8], b: &[u8]) -> Moves {
    let budget = Budget::new(Bytes(64 << 20));
    let (_, _, map) = build_pair(a, b, &budget).unwrap();
    let (ma, mb) = (MemLines::new(a), MemLines::new(b));
    search(map.off_chain().pairs(), map.segments(), &ma, &mb, &budget)
}

fn block(a: Range<u64>, b: Range<u64>) -> MovedBlock {
    MovedBlock {
        a: LineNo(a.start)..LineNo(a.end),
        b: LineNo(b.start)..LineNo(b.end),
    }
}

/// `n` unique lines with lines `from..from + len` moved to land before
/// line `to` of the rest.
fn moved(n: u64, from: u64, len: u64, to: u64) -> (Vec<u8>, Vec<u8>) {
    let line = |i: u64| format!("row {i} of the table\n");
    let a = rows(n, line);
    let rest: Vec<u64> = (0..n).filter(|i| !(from..from + len).contains(i)).collect();
    let mut order: Vec<u64> = rest[..usize::try_from(to).unwrap()].to_vec();
    order.extend(from..from + len);
    order.extend(&rest[usize::try_from(to).unwrap()..]);
    let b = order.iter().flat_map(|&i| line(i).into_bytes()).collect();
    (a, b)
}

#[test]
fn a_block_moved_down_is_found_with_its_exact_ends() {
    let (a, b) = moved(1000, 100, 20, 600);
    let m = moves(&a, &b);
    assert!(!m.skipped());
    assert_eq!(m.blocks(), [block(100..120, 600..620)]);
}

#[test]
fn a_block_moved_up_is_found() {
    let (a, b) = moved(1000, 700, 8, 50);
    assert_eq!(moves(&a, &b).blocks(), [block(700..708, 50..58)]);
}

#[test]
fn edits_without_moves_report_none() {
    let a = rows(5000, |i| format!("row {i}\n"));
    let b = rows(5000, |i| match i % 101 {
        3 => String::new(),
        7 => format!("row {i} changed\n"),
        9 => format!("row {i}\nadded {i}\n"),
        _ => format!("row {i}\n"),
    });
    assert_eq!(moves(&a, &b).blocks(), []);
}

#[test]
fn a_run_shorter_than_the_minimum_is_not_a_move() {
    let (a, b) = moved(1000, 100, MIN_LINES - 1, 600);
    assert_eq!(moves(&a, &b).blocks(), []);
    let (a, b) = moved(1000, 100, MIN_LINES, 600);
    assert_eq!(moves(&a, &b).blocks().len(), 1);
}

/// One `Differs` segment over everything, and the given pairs.
fn find(a: &[u8], b: &[u8], pairs: &[[LineNo; 2]], budget: &Budget) -> Moves {
    let (ma, mb) = (MemLines::new(a), MemLines::new(b));
    let seg = Segment {
        a: LineNo(0)..LineNo(ma.line_count()),
        b: LineNo(0)..LineNo(mb.line_count()),
        status: SegmentStatus::Differs,
    };
    search(pairs, &[seg], &ma, &mb, budget)
}

#[test]
fn a_pair_whose_lines_differ_is_not_a_seed() {
    // A hash match only: the pair points at lines whose bytes differ, but
    // their neighbours match.
    let a = b"x\n1\n2\n3\n4\n5\n";
    let b = b"y\n1\n2\n3\n4\n5\n";
    let budget = Budget::new(Bytes(16 << 20));
    let at = |a, b| [LineNo(a), LineNo(b)];
    assert_eq!(find(a, b, &[at(0, 0)], &budget).blocks(), []);
    assert_eq!(
        find(a, b, &[at(0, 0), at(3, 3)], &budget).blocks(),
        [block(1..6, 1..6)]
    );
}

#[test]
fn a_run_stops_at_an_equal_segment() {
    // Lines 0..3 of A are the chain's, aligned with B's 0..3; the copy of
    // them at B 6..9 extends no further back than the moved line 3.
    let a = b"p\nq\nr\ns\nt\nu\n";
    let b = b"p\nq\nr\nz\nz\nz\np\nq\nr\ns\n";
    let segs = [
        Segment {
            a: LineNo(0)..LineNo(3),
            b: LineNo(0)..LineNo(3),
            status: SegmentStatus::Equal,
        },
        Segment {
            a: LineNo(3)..LineNo(6),
            b: LineNo(3)..LineNo(10),
            status: SegmentStatus::Differs,
        },
    ];
    let (ma, mb) = (MemLines::new(a), MemLines::new(b));
    let budget = Budget::new(Bytes(16 << 20));
    let pairs = [[LineNo(3), LineNo(9)]];
    let m = search(&pairs, &segs, &ma, &mb, &budget);
    assert_eq!(m.blocks(), []);
}

#[test]
fn a_budget_too_small_for_the_buffers_skips_moves() {
    let (a, b) = moved(1000, 100, 20, 600);
    let budget = Budget::new(Bytes(64 << 20));
    let (_, _, map) = build_pair(&a[..], &b[..], &budget).unwrap();
    let (ma, mb) = (MemLines::new(&a), MemLines::new(&b));
    let pairs = map.off_chain().pairs();
    for total in [work(CAP).0, 2 * work(CAP).0] {
        let tiny = Budget::new(Bytes(total));
        let m = search(pairs, map.segments(), &ma, &mb, &tiny);
        assert!(m.skipped());
        assert_eq!(m.blocks(), []);
        assert_eq!(
            tiny.used(Half::Refine),
            Moves::list_bytes(pairs.len(), &tiny)
        );
        drop(m);
        assert_eq!(tiny.used(Half::Refine), Bytes(0));
    }
}

#[test]
fn the_list_stays_reserved_whole_until_dropped() {
    let (a, b) = moved(1000, 100, 20, 600);
    let budget = Budget::new(Bytes(64 << 20));
    let (_, _, map) = build_pair(&a[..], &b[..], &budget).unwrap();
    let (ma, mb) = (MemLines::new(&a), MemLines::new(&b));
    let before = budget.used(Half::Refine);
    let pairs = map.off_chain().pairs();
    let m = search(pairs, map.segments(), &ma, &mb, &budget);
    assert_eq!(m.blocks().len(), 1);
    assert_eq!(
        Bytes(budget.used(Half::Refine).0 - before.0),
        Moves::list_bytes(pairs.len(), &budget)
    );
    drop(m);
    assert_eq!(budget.used(Half::Refine), before);
}

/// Ten pairs of five-line runs, swapped in B, each pair between lines that
/// stay: one moved block per pair.
fn swapped_runs() -> (Vec<u8>, Vec<u8>) {
    let run = |s: u64, k: u64| rows(5, |i| format!("row {s} {k} {i}\n"));
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for s in 0..10 {
        let sep = format!("stays {s}\n").into_bytes();
        a.extend([run(s, 0), run(s, 1), sep.clone()].concat());
        b.extend([run(s, 1), run(s, 0), sep].concat());
    }
    (a, b)
}

#[test]
fn a_full_list_keeps_the_blocks_found_first() {
    let (a, b) = swapped_runs();
    let all = moves(&a, &b);
    assert!(all.blocks().len() > 4, "{:?}", all.blocks());
    assert!(!all.full());
    let budget = Budget::new(Bytes(64 << 20));
    let (_, _, map) = build_pair(&a[..], &b[..], &budget).unwrap();
    let (ma, mb) = (MemLines::new(&a), MemLines::new(&b));
    let list = budget
        .try_reserve(Half::Refine, Bytes(4 * BLOCK_BYTES))
        .ok();
    let pairs = map.off_chain().pairs();
    let m = Moves::find(pairs, map.segments(), &ma, &mb, CAP, &budget, list).unwrap();
    assert!(m.full() && !m.skipped());
    assert_eq!(m.blocks(), &all.blocks()[..4]);
}
