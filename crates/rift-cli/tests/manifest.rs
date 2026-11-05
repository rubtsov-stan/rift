//! The segment map of generated pairs against the generator's manifest.

use std::ops::Range;

use rift_core::skeleton::build_pair;
use rift_core::{Budget, Bytes, LineAccess, LineNo, MemLines, Segment, SegmentStatus};
use rift_gen::{Config, Kind, Mutation, Profile};

const ALL: [Kind; 9] = [
    Kind::Insert,
    Kind::Delete,
    Kind::Modify,
    Kind::Move,
    Kind::Dup,
    Kind::LowUniq,
    Kind::Long,
    Kind::Crlf,
    Kind::NoEol,
];

struct Pair {
    a: Vec<u8>,
    b: Vec<u8>,
    manifest: Vec<Mutation>,
}

fn generate(kinds: &[Kind], seed: u64, profile: Profile) -> Pair {
    let long = kinds.contains(&Kind::Long);
    let mut config = Config::new(seed, if long { 8 << 20 } else { 1 << 20 }, profile);
    config.kinds = kinds.to_vec();
    config.change_ratio = if long { 0.02 } else { 0.005 };
    let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
    rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
    let manifest = rift_gen::parse_manifest(&m).unwrap();
    Pair { a, b, manifest }
}

/// The first seed whose manifest has a row of `kind`.
fn generate_with(kind: Kind, kinds: &[Kind], profile: Profile) -> Pair {
    (1..50)
        .map(|seed| generate(kinds, seed, profile))
        .find(|p| p.manifest.iter().any(|m| m.kind == kind))
        .unwrap_or_else(|| panic!("no seed produced {kind:?}"))
}

fn lines(r: &Range<LineNo>) -> Range<u64> {
    r.start.0..r.end.0
}

fn overlaps(x: &Range<u64>, y: &Range<u64>) -> bool {
    x.start < y.end && y.start < x.end
}

/// Like `overlaps`, but an empty range counts at its position.
fn touches(x: &Range<u64>, y: &Range<u64>) -> bool {
    x.start <= y.end && y.start <= x.end
}

/// Lines each mutation changes in A and in B.
fn changed(m: &Mutation) -> (Range<u64>, Range<u64>) {
    let (a, b) = (m.line_a..m.line_a + m.count, m.line_b..m.line_b + m.count);
    match m.kind {
        Kind::Insert => (m.line_a..m.line_a, b),
        Kind::Delete => (a, m.line_b..m.line_b),
        _ => (a, b),
    }
}

fn check(pair: &Pair, budget: Bytes) {
    let (_, _, map) = build_pair(&pair.a[..], &pair.b[..], &Budget::new(budget)).unwrap();
    let segments = map.segments();
    let (ma, mb) = (MemLines::new(&pair.a), MemLines::new(&pair.b));
    tiles_and_equal_is_equal(&ma, &mb, segments);

    let differing: Vec<&Mutation> = pair.manifest.iter().filter(|m| m.kind.differs()).collect();
    let open: Vec<&Segment> = segments
        .iter()
        .filter(|s| s.status != SegmentStatus::Equal)
        .collect();
    assert_eq!(map.is_identical(), differing.is_empty());
    if differing.is_empty() {
        assert_eq!(segments.len(), usize::from(!pair.a.is_empty()));
    }

    for m in &differing {
        let (a, b) = changed(m);
        let covered = |r: &Range<u64>, side: fn(&Segment) -> Range<u64>| -> u64 {
            open.iter()
                .map(|s| {
                    let s = side(s);
                    s.end.min(r.end).saturating_sub(s.start.max(r.start))
                })
                .sum()
        };
        // A moved block may be kept in place while what it jumped over is
        // marked instead, so only its neighbourhood must differ.
        if m.kind == Kind::Move {
            assert!(
                open.iter().any(|s| touches(&lines(&s.a), &a)),
                "nothing differs around {m:?}"
            );
        } else {
            assert_eq!(covered(&a, |s| lines(&s.a)), a.end - a.start, "{m:?} in A");
            assert_eq!(covered(&b, |s| lines(&s.b)), b.end - b.start, "{m:?} in B");
        }
    }

    for s in &open {
        let (sa, sb) = (lines(&s.a), lines(&s.b));
        let justified = differing.iter().any(|m| {
            let (a, b) = changed(m);
            let near = if m.kind == Kind::Move {
                touches
            } else {
                overlaps
            };
            near(&sa, &a)
                || near(&sb, &b)
                || (a.is_empty() && touches(&sa, &a))
                || (b.is_empty() && touches(&sb, &b))
        });
        assert!(justified, "{s:?} differs without a mutation");
    }
}

fn tiles_and_equal_is_equal(ma: &MemLines<'_>, mb: &MemLines<'_>, segments: &[Segment]) {
    let bytes = |m: &MemLines<'_>, r: Range<u64>| -> Vec<u8> {
        r.flat_map(|i| m.line(i).unwrap().to_vec()).collect()
    };
    let (mut a, mut b) = (0, 0);
    for s in segments {
        assert_eq!((s.a.start.0, s.b.start.0), (a, b), "gap at {s:?}");
        let same = bytes(ma, lines(&s.a)) == bytes(mb, lines(&s.b));
        assert_eq!(same, s.status == SegmentStatus::Equal, "{s:?}");
        (a, b) = (s.a.end.0, s.b.end.0);
    }
    assert_eq!((a, b), (ma.line_count(), mb.line_count()));
}

#[test]
fn every_kind_alone_matches_its_manifest() {
    for kind in ALL {
        let pair = generate_with(kind, &[kind], Profile::Sql);
        for budget in [Bytes::mib(4), Bytes::mib(64)] {
            check(&pair, budget);
        }
    }
}

#[test]
fn all_kinds_together_match_the_manifest() {
    for profile in [Profile::Sql, Profile::Csv, Profile::Ndjson, Profile::Text] {
        for seed in 1..4 {
            let pair = generate(&ALL, seed, profile);
            check(&pair, Bytes::mib(8));
        }
    }
}
