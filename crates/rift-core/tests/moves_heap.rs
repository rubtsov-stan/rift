//! Moved-block detection allocates no more than it reserves from the
//! refine half, and gives back everything but the block list.
//!
//! Alone in its binary because it reads the process-wide allocator counters.

use rift_core::alloc::{self, CountingAlloc};
use rift_core::index::IndexedLines;
use rift_core::moves::Moves;
use rift_core::skeleton::build_pair;
use rift_core::{Budget, Bytes, Half};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Line `i`: its number, then filler up to `len` bytes.
fn line(i: usize, len: usize) -> Vec<u8> {
    let mut l = format!("{i:08} ").into_bytes();
    l.resize(len - 1, b"abcdefghijklmnopqrstuvwxyz"[i % 26]);
    l.push(b'\n');
    l
}

/// What the readers return whole.
const CAP: Bytes = Bytes(4 << 10);

/// Detection over 40 lines of `len` bytes, lines 10..14 moved to the end,
/// at the smallest refine half, in steps of 4 KiB, that holds it: the heap
/// it takes stays within that half, and it keeps only its block.
fn check(len: usize) {
    let a: Vec<u8> = (0..40).flat_map(|i| line(i, len)).collect();
    let order = (0..10).chain(14..40).chain(10..14);
    let b: Vec<u8> = order.flat_map(|i| line(i, len)).collect();
    let other = Budget::new(Bytes(256 << 20));
    let (scan_a, scan_b, map) = build_pair(&a[..], &b[..], &other).unwrap();
    let ra = IndexedLines::new(&a[..], scan_a.index(), &other, Half::Refine, CAP).unwrap();
    let rb = IndexedLines::new(&b[..], scan_b.index(), &other, Half::Refine, CAP).unwrap();
    let pairs = map.off_chain().pairs();

    let (moves, budget, peak) = (1..)
        .map(|k| Budget::new(Bytes(2 * k * (4 << 10))))
        .find_map(|budget| {
            let before = alloc::current();
            alloc::reset_peak();
            let list = Moves::list_bytes(pairs.len(), &budget);
            let list = budget.try_reserve(Half::Refine, list).ok();
            let m = Moves::find(pairs, map.segments(), &ra, &rb, CAP, &budget, list).unwrap();
            let peak = Bytes(alloc::peak().0 - before.0);
            (!m.skipped()).then_some((m, budget, peak))
        })
        .unwrap();
    assert_eq!(moves.blocks().len(), 1, "{len}");
    let half = budget.capacity(Half::Refine);
    assert!(peak <= half, "{len}: peak {peak} above the half of {half}");
    let held = budget.used(Half::Refine);
    let list = Moves::list_bytes(pairs.len(), &budget);
    assert_eq!(held, list, "{len}: {held} held after detection");
    drop(moves);
    assert_eq!(budget.used(Half::Refine), Bytes(0));
}

#[test]
fn heap_stays_within_the_reservation() {
    // Blocks of whole lines just under the readers' buffer, and lines
    // compared in pieces.
    check(4000);
    check(200 << 10);
}
