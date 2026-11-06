//! The diff of a small generated pair replays A into B.

use std::sync::Arc;

use rift_core::diff::Diff;
use rift_core::{Budget, ByteOffset, Bytes, DiffView, HunkState, MemLines, Source, script};
use rift_gen::synthetic::{Side, Synthetic};

const SEED: u64 = 7;
const CHANGE: f64 = 0.001;

fn pair(size: u64) -> (Arc<Synthetic>, Arc<Synthetic>) {
    (
        Arc::new(Synthetic::new(SEED, size, CHANGE, Side::A)),
        Arc::new(Synthetic::new(SEED, size, CHANGE, Side::B)),
    )
}

fn whole(s: &Synthetic) -> Vec<u8> {
    let mut v = vec![0u8; usize::try_from(s.len().0).unwrap()];
    s.read_exact_at(&mut v, ByteOffset(0)).unwrap();
    v
}

#[test]
fn the_diff_of_a_small_synthetic_pair_replays_to_b() {
    let (a, b) = pair(8 << 20);
    let (da, db) = (whole(&a), whole(&b));
    let budget = Budget::new(Bytes::mib(64));
    let diff = Diff::new(Arc::clone(&a), Arc::clone(&b), &budget, 4).unwrap();
    let hunks: Vec<_> = (0..diff.segments().len())
        .map(|i| {
            loop {
                match diff.hunk(i) {
                    HunkState::Ready(h) | HunkState::Approx(h) => break (*h).clone(),
                    HunkState::Pending => std::thread::yield_now(),
                }
            }
        })
        .collect();
    let mut out = Vec::new();
    script::apply(&MemLines::new(&da), &MemLines::new(&db), &hunks, &mut out).unwrap();
    assert!(out == db, "script replays A into B");
    assert!(
        diff.segments().len() > 10,
        "the pair differs in many places"
    );
}
