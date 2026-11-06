//! Diffs of pairs without unique lines, where anchors come from shingles.

#![allow(clippy::cast_possible_truncation, reason = "test inputs are small")]

use std::sync::Arc;
use std::time::Duration;

use proptest::prelude::*;
use rift_core::diff::Diff;
use rift_core::script::{apply, validate};
use rift_core::{Budget, Bytes, DiffView, HunkState, LineAccess, LineNo, MemLines};

/// Rows drawn from a pool of `pool` rows, so no row is unique once the
/// file is a few times longer than the pool.
fn rows(picks: impl IntoIterator<Item = u32>) -> Vec<u8> {
    picks
        .into_iter()
        .flat_map(|p| format!("{},{},{}\n", p % 7, p / 7, p % 3).into_bytes())
        .collect()
}

/// A deterministic stream of picks from `pool` rows.
fn picks(n: usize, pool: u32, seed: u64) -> Vec<u32> {
    let mut x = seed;
    (0..n)
        .map(|_| {
            x = x
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            ((x >> 33) % u64::from(pool)) as u32
        })
        .collect()
}

/// Diffs the pair, checks that the script replays A into B, and returns
/// the lines of A and B in approximate hunks.
fn approx_lines(a: &[u8], b: &[u8], mem: Bytes) -> u64 {
    let d = Diff::new(
        Arc::<[u8]>::from(a),
        Arc::<[u8]>::from(b),
        &Budget::new(mem),
        2,
    )
    .unwrap();
    d.set_visible(LineNo(0)..LineNo(d.len_a()));
    let mut hunks = Vec::new();
    let mut approx = 0;
    for (i, seg) in d.segments().iter().enumerate() {
        let hunk = loop {
            match d.hunk(i) {
                HunkState::Ready(h) => break h,
                HunkState::Approx(h) => {
                    approx += (seg.a.end - seg.a.start) + (seg.b.end - seg.b.start);
                    break h;
                }
                HunkState::Pending => std::thread::sleep(Duration::from_millis(1)),
            }
        };
        hunks.push(hunk);
    }
    let (ma, mb) = (MemLines::new(a), MemLines::new(b));
    let hunks: Vec<_> = hunks.iter().map(|h| &**h).collect();
    validate(hunks.iter().copied(), ma.line_count(), mb.line_count()).unwrap();
    let mut out = Vec::new();
    apply(&ma, &mb, hunks.iter().copied(), &mut out).unwrap();
    assert_eq!(out, b, "script replays A into B");
    approx
}

/// `a` with a few rows changed, deleted and inserted.
fn mutate(a: &[u32], edits: &[(usize, u8)], pool: u32) -> Vec<u32> {
    let mut b = a.to_vec();
    for &(at, kind) in edits {
        let at = at % (b.len() + 1);
        match kind % 3 {
            0 if at < b.len() => b[at] = (b[at] + 1) % pool,
            1 if at < b.len() => drop(b.remove(at)),
            _ => b.insert(at, (at as u32) % pool),
        }
    }
    b
}

#[test]
fn a_pair_without_unique_lines_gets_an_exact_script() {
    let pool = 64;
    let a = picks(400_000, pool, 7);
    let edits: Vec<(usize, u8)> = (0..200).map(|i| (i * 1999 + 17, (i % 3) as u8)).collect();
    let b = mutate(&a, &edits, pool);
    let (a, b) = (rows(a), rows(b));
    assert_eq!(approx_lines(&a, &b, Bytes::mib(32)), 0);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(32))]

    /// Small pools leave no unique line; tiny budgets force thinning and the
    /// fallback to unique lines alone.
    #[test]
    fn pairs_from_a_small_pool_replay(
        seed in any::<u64>(),
        n in 0usize..20_000,
        pool in prop::sample::select(vec![2u32, 4, 16, 256]),
        edits in prop::collection::vec((any::<usize>(), any::<u8>()), 0..40),
        mem in prop::sample::select(vec![16u64 << 20, 24 << 20, 64 << 20]),
    ) {
        let a = picks(n, pool, seed);
        let b = mutate(&a, &edits, pool);
        approx_lines(&rows(a), &rows(b), Bytes(mem));
    }
}
