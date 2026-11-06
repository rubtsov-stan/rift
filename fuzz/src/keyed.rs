//! Key mode: the CSV, NDJSON and `COPY` record readers, reached through
//! `rift_core::keyed::compare` (the readers themselves are private).

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Once;

use rift_core::keyed::{ChangeKind, KeyError, KeySpec, KeyedChange, Report, compare};
use rift_core::{Budget, Bytes, Error, Result};

use crate::{config, fields, peak_heap};

const SMALL: [u64; 4] = [64 << 10, 256 << 10, 1 << 20, 4 << 20];
const LARGE: Bytes = Bytes::mib(64);

static WARM: Once = Once::new();

/// CSV keys: column 1, column 2, or the column named `id`.
pub fn csv(data: &[u8]) {
    check(data, &["csv:1", "csv:2", "csv:id"]);
}

/// NDJSON keys: the field `id` or the path `actor.id`.
pub fn ndjson(data: &[u8]) {
    check(data, &["json:id", "json:actor.id"]);
}

/// `COPY` keys: the column `id` or `rental_id`.
pub fn copy(data: &[u8]) {
    check(data, &["copy:id", "copy:rental_id"]);
}

/// What a run reported: its changes folded into a count and an
/// order-free hash, so that collecting them costs no heap.
#[derive(Debug, Default, PartialEq, Eq)]
struct Seen {
    count: u64,
    sum: u64,
}

fn run(a: &[u8], b: &[u8], spec: &KeySpec, mem: Bytes) -> (Result<Report>, Seen) {
    // Counted without allocating: this runs inside the heap measurement.
    let lines = |f: &[u8]| f.split_inclusive(|&c| c == b'\n').count() as u64;
    let (lines_a, lines_b) = (lines(a), lines(b));
    let mut seen = Seen::default();
    let res = compare(a, b, spec, &Budget::new(mem), |c: &KeyedChange| {
        match c.kind {
            ChangeKind::Added => assert!(c.a.is_none() && c.b.is_some(), "{c:?}"),
            ChangeKind::Removed => assert!(c.a.is_some() && c.b.is_none(), "{c:?}"),
            ChangeKind::Changed => assert!(c.a.is_some() && c.b.is_some(), "{c:?}"),
        }
        assert!(c.a.is_none_or(|l| l.0 < lines_a), "{c:?} past the end of a");
        assert!(c.b.is_none_or(|l| l.0 < lines_b), "{c:?} past the end of b");
        let mut h = DefaultHasher::new();
        c.hash(&mut h);
        seen.count += 1;
        seen.sum = seen.sum.wrapping_add(h.finish());
        Ok(())
    });
    (res, seen)
}

/// Errors that depend on the budget, not on the input.
fn budget_bound(e: &Error) -> bool {
    matches!(
        e,
        Error::Budget(_) | Error::LineTooLong { .. } | Error::Keyed(KeyError::Collisions)
    )
}

/// Compares A with B under a small and a large budget, and A with itself.
///
/// Properties: the reported changes are well formed; the two budgets
/// report the same set of changes when both succeed; when one succeeds,
/// the other fails only for a budget reason (so a duplicate or missing key
/// is never seen under one budget and missed under the other); A against
/// itself reports nothing; the small run's peak heap stays within its
/// budget.
fn check(data: &[u8], specs: &[&str]) {
    let (cfg, rest) = config(data);
    let [a, b] = fields(rest);
    let spec: KeySpec = specs[usize::from(cfg & 3) % specs.len()]
        .parse()
        .expect("spec");
    let small = Bytes(SMALL[usize::from(cfg >> 2 & 3)]);
    // The first compare builds rayon's global pool; charge it to no input.
    WARM.call_once(|| drop(run(b"id\n1\n", b"id\n2\n", &spec, LARGE)));

    let ((res_small, seen_small), heap) = peak_heap(|| run(a, b, &spec, small));
    assert!(heap <= small, "peak heap {heap} over the budget {small}");
    let (res_large, seen_large) = run(a, b, &spec, LARGE);
    match (&res_small, &res_large) {
        (Ok(rs), Ok(rl)) => {
            assert_eq!(seen_small, seen_large, "changes depend on the budget");
            assert_eq!(rs.changes, seen_small.count);
            assert_eq!(rl.changes, seen_large.count);
            assert_eq!(
                (rs.records_a, rs.records_b),
                (rl.records_a, rl.records_b),
                "record counts depend on the budget"
            );
        }
        (Ok(_), Err(e)) | (Err(e), Ok(_)) => {
            assert!(budget_bound(e), "one budget succeeds, the other fails: {e}");
        }
        (Err(_), Err(_)) => {}
    }

    if let (Ok(_), seen) = run(a, a, &spec, LARGE) {
        assert_eq!(seen, Seen::default(), "a file differs from itself");
    }
}
