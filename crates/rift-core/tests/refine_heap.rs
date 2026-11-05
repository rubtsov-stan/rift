//! Peak heap of one refinement stays within `work_estimate`.
//!
//! Alone in its binary because it reads the process-wide allocator counters.

use rift_core::alloc::{self, CountingAlloc};
use rift_core::refine::{Refiner, work_estimate};
use rift_core::{Budget, Bytes, Edit, Half, LineAccess, LineNo, MemLines, Segment, SegmentStatus};
use xxhash_rust::xxh3::xxh3_64;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// `n` unique lines, with about `rate` of them changed in B.
fn pair(n: usize, rate: f64, seed: u64) -> (Vec<u8>, Vec<u8>) {
    let mut rng = Rng(seed);
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let mut op = 0;
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "rate is a small positive fraction"
    )]
    let every = (1.0 / rate) as u64;
    for i in 0..n {
        let line = format!("{i}\t{:016x}\tvalue\n", rng.next());
        a.extend_from_slice(line.as_bytes());
        if rng.next().is_multiple_of(every) {
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

fn hashes(data: &[u8]) -> Vec<u64> {
    let m = MemLines::new(data);
    (0..m.line_count())
        .map(|i| xxh3_64(m.line(i).unwrap()))
        .collect()
}

#[test]
fn heap_stays_within_the_estimate() {
    let (a, b) = pair(100_000, 0.001, 11);
    let (ma, mb) = (MemLines::new(&a), MemLines::new(&b));
    let (ha, hb) = (hashes(&a), hashes(&b));
    let seg = Segment {
        a: LineNo(0)..LineNo(ha.len() as u64),
        b: LineNo(0)..LineNo(hb.len() as u64),
        status: SegmentStatus::Differs,
    };
    let est = work_estimate(ha.len() as u64, hb.len() as u64);
    let budget = Budget::new(Bytes::mib(256));
    let mut res = budget.try_reserve(Half::Refine, est).unwrap();
    let mut r = Refiner::new(1000);
    // First call grows the line buffers; the second is measured.
    r.refine(&seg, &ha, &hb, &ma, &mb, &mut res).unwrap();
    let before = alloc::current().0;
    alloc::reset_peak();
    let hunk = r.refine(&seg, &ha, &hb, &ma, &mb, &mut res).unwrap();
    let peak = alloc::peak().0 - before;
    let hunk_bytes = (hunk.edits.capacity() * size_of::<Edit>()) as u64;
    assert!(
        peak <= est.0 + hunk_bytes,
        "peak {peak} > estimate {} + hunk {hunk_bytes}",
        est.0
    );
}
