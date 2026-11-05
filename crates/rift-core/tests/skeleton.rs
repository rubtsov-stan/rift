//! Anchors and the segment map against in-memory pairs.

use proptest::prelude::*;
use rift_core::scan::{ScanConfig, scan};
use rift_core::skeleton::{Skeleton, build, build_pair};
use rift_core::{Budget, Bytes, LineAccess, LineNo, MemLines, Segment, SegmentStatus, Source};

fn config(level: u32) -> ScanConfig {
    ScanConfig {
        level,
        sample_cap: 1 << 14,
        checkpoint_interval: 16,
        checkpoint_cap: 1 << 12,
        chunk: Bytes(4096),
        threads: 2,
    }
}

fn map_at(a: &[u8], b: &[u8], level: u32) -> Skeleton {
    let budget = Budget::new(Bytes::mib(16));
    let mut sa = scan(a, &budget, &config(level)).unwrap();
    let mut sb = scan(b, &budget, &config(level)).unwrap();
    let map = build(a, b, &mut sa, &mut sb, &budget).unwrap();
    check(a, b, map.segments());
    map
}

fn bytes(m: &MemLines<'_>, lines: &std::ops::Range<LineNo>) -> Vec<u8> {
    (lines.start.0..lines.end.0)
        .flat_map(|i| m.line(i).unwrap().iter().copied())
        .collect()
}

/// The segments tile both files, `Equal` ones hold the same bytes on both
/// sides and `Differs` ones do not.
fn check(a: &[u8], b: &[u8], segments: &[Segment]) {
    let (ma, mb) = (MemLines::new(a), MemLines::new(b));
    let (mut la, mut lb) = (LineNo(0), LineNo(0));
    for s in segments {
        assert_eq!((s.a.start, s.b.start), (la, lb), "gap or overlap at {s:?}");
        assert!(s.len_a() + s.len_b() > 0, "empty segment");
        let same = bytes(&ma, &s.a) == bytes(&mb, &s.b);
        match s.status {
            SegmentStatus::Equal => assert!(same, "{s:?} is not equal"),
            _ => assert!(!same, "{s:?} is equal"),
        }
        (la, lb) = (s.a.end, s.b.end);
    }
    assert_eq!(la.0, ma.line_count());
    assert_eq!(lb.0, mb.line_count());
}

fn lines(n: usize) -> Vec<u8> {
    (0..n)
        .flat_map(|i| format!("row {i} value {}\n", i * 7919 % 1000).into_bytes())
        .collect()
}

#[test]
fn identical_files_are_one_equal_segment() {
    let data = lines(5000);
    for level in [0, 3, 20] {
        let map = map_at(&data, &data, level);
        assert_eq!(map.segments().len(), 1);
        assert!(map.is_identical());
    }
}

#[test]
fn empty_files_have_no_segments() {
    let map = map_at(b"", b"", 0);
    assert!(map.segments().is_empty());
    assert!(map.is_identical());
    assert_eq!(map.anchors(), 0);
}

#[test]
fn one_empty_file_is_one_differing_segment() {
    let data = lines(10);
    for (a, b) in [(&data[..], &b""[..]), (b"", &data[..])] {
        let map = map_at(a, b, 0);
        assert_eq!(map.segments().len(), 1);
        assert_eq!(map.segments()[0].status, SegmentStatus::Differs);
    }
}

#[test]
fn single_lines() {
    assert!(map_at(b"x\n", b"x\n", 0).is_identical());
    assert!(map_at(b"x", b"x", 0).is_identical());
    assert!(!map_at(b"x", b"x\n", 0).is_identical());
    assert!(!map_at(b"x\n", b"y\n", 0).is_identical());
}

#[test]
fn a_change_is_isolated_between_anchors() {
    let a = lines(1000);
    let mut b = a.clone();
    let at = a.len() / 2;
    b[at] ^= 1;
    let map = map_at(&a, &b, 0);
    let differs: Vec<_> = map
        .segments()
        .iter()
        .filter(|s| s.status != SegmentStatus::Equal)
        .collect();
    assert_eq!(differs.len(), 1);
    assert_eq!(map.segments().len(), 3);
    assert!(differs[0].len_a() <= 2, "{:?}", differs[0]);
}

#[test]
fn inserted_and_deleted_blocks_keep_the_rest_equal() {
    let a = lines(3000);
    let cut = a.len() / 3;
    let mut b = a[..cut].to_vec();
    b.extend_from_slice(b"inserted\nlines\n");
    b.extend_from_slice(&a[cut + 500..]);
    let map = map_at(&a, &b, 1);
    let equal: u64 = map
        .segments()
        .iter()
        .filter(|s| s.status == SegmentStatus::Equal)
        .map(Segment::len_a)
        .sum();
    assert!(equal > 2900, "only {equal} equal lines");
}

#[test]
fn a_tight_budget_thins_until_the_table_fits() {
    let a = lines(20_000);
    let mut b = a.clone();
    b.truncate(b.len() - 1000);
    let budget = Budget::new(Bytes(3_000_000));
    let mut cfg = config(0);
    cfg.sample_cap = 20_000;
    let mut sa = scan(&a[..], &budget, &cfg).unwrap();
    let mut sb = scan(&b[..], &budget, &cfg).unwrap();
    assert_eq!(sa.level(), 0);
    let map = build(&a[..], &b[..], &mut sa, &mut sb, &budget).unwrap();
    check(&a, &b, map.segments());
    assert!(
        map.level() > 0,
        "the table cannot fit beside 20 000 samples per file"
    );
    assert_eq!(sa.level(), map.level());
}

#[test]
fn build_pair_sizes_itself_from_the_budget() {
    let a = lines(20_000);
    let b = [&a[..], b"tail\n"].concat();
    let (_, _, map) = build_pair(&a[..], &b[..], &Budget::new(Bytes::mib(4))).unwrap();
    check(&a, &b, map.segments());
    assert_eq!(map.segments().len(), 2);
}

#[test]
fn repeated_lines_without_anchors_still_compare() {
    let a = b"same\n".repeat(100);
    let map = map_at(&a, &a, 0);
    assert_eq!(map.anchors(), 0);
    assert!(map.is_identical());
    let b = b"same\n".repeat(101);
    assert!(!map_at(&a, &b, 0).is_identical());
}

proptest! {
    #[test]
    fn map_tiles_and_equal_means_equal(
        base in prop::collection::vec("[a-e]{0,4}\r?", 0..60),
        edits in prop::collection::vec((0..61usize, 0..3u8, "[a-f]{0,3}"), 0..8),
        level in 0u32..3,
        tail in any::<bool>(),
    ) {
        let mut other = base.clone();
        for (at, op, text) in &edits {
            let at = (*at).min(other.len());
            match op {
                0 => other.insert(at, text.clone()),
                1 if at < other.len() => { other.remove(at); }
                _ if at < other.len() => other[at].clone_from(text),
                _ => {}
            }
        }
        let join = |ls: &[String]| {
            let mut v: Vec<u8> = ls.iter().flat_map(|l| format!("{l}\n").into_bytes()).collect();
            if tail { v.pop(); }
            v
        };
        let (a, b) = (join(&base), join(&other));
        let map = map_at(&a, &b, level);
        prop_assert_eq!(map.is_identical(), a == b);
    }
}

/// A source whose bytes change once the scan is done, so a line that hashed
/// equal to its counterpart reads back different: a forced hash collision.
struct Collide {
    scanned: Vec<u8>,
    read_later: Vec<u8>,
    swapped: std::sync::atomic::AtomicBool,
}

impl Source for Collide {
    fn len(&self) -> Bytes {
        Bytes(self.scanned.len() as u64)
    }

    fn read_at(&self, buf: &mut [u8], offset: rift_core::ByteOffset) -> std::io::Result<usize> {
        let swapped = self.swapped.load(std::sync::atomic::Ordering::Relaxed);
        let data = if swapped {
            &self.read_later
        } else {
            &self.scanned
        };
        data[..].read_at(buf, offset)
    }
}

#[test]
fn a_false_anchor_is_demoted_and_never_equal() {
    let a = lines(400);
    let ma = MemLines::new(&a);
    // Line 200 of B scans like A's line 200, then reads back changed.
    let k = 200;
    let start: usize = (0..k).map(|i| ma.line(i).unwrap().len()).sum();
    let mut later = a.clone();
    later[start] = b'#';
    let b = Collide {
        scanned: a.clone(),
        read_later: later.clone(),
        swapped: false.into(),
    };
    let budget = Budget::new(Bytes::mib(16));
    let mut sa = scan(&a[..], &budget, &config(0)).unwrap();
    let mut sb = scan(&b, &budget, &config(0)).unwrap();
    b.swapped.store(true, std::sync::atomic::Ordering::Relaxed);
    let map = build(&a[..], &b, &mut sa, &mut sb, &budget).unwrap();

    check(&a, &later, map.segments());
    let differs: Vec<&Segment> = map
        .segments()
        .iter()
        .filter(|s| s.status != SegmentStatus::Equal)
        .collect();
    // Every line is an anchor at stratum 0. The gap the false anchor opens
    // merges with the gap before it.
    assert_eq!(differs.len(), 1, "{:?}", map.segments());
    assert_eq!(differs[0].a, LineNo(k - 1)..LineNo(k + 1));
    assert_eq!(differs[0].b, LineNo(k - 1)..LineNo(k + 1));
    assert_eq!(map.anchors(), 399);
}
