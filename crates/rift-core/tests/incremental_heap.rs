//! A guided re-diff stays within `--mem`, for random pairs and appends.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters.

use std::sync::Arc;

use proptest::prelude::*;
use rift_core::alloc::{self, CountingAlloc};
use rift_core::diff::{Config, Diff};
use rift_core::incremental::{self, Guide, Previous};
use rift_core::scan::Watch;
use rift_core::unified::{Label, Options};
use rift_core::{Budget, Bytes, DiffView};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

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

fn rows(seed: u64, n: u64, modulo: u64) -> Vec<u8> {
    (0..n)
        .flat_map(|i| format!("row {} {}\n", (i * 7919 + seed) % modulo, i % 13).into_bytes())
        .collect()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn the_guided_run_keeps_its_peak_under_the_budget(
        seed in 0u64..1000,
        n in 1_000u64..40_000,
        modulo in prop::sample::select(vec![50u64, 5_000, 1 << 40]),
        tail in 0u64..8_000,
        edit in 0usize..200,
        mem in prop::sample::select(vec![11u64 << 20, 16 << 20, 64 << 20]),
    ) {
        let a: Arc<[u8]> = rows(seed, n, modulo).into();
        let mut b = a.to_vec();
        let at = (edit * 37) % b.len().max(1);
        let at = at + b[at..].iter().position(|&c| c == b'\n').map_or(0, |k| k + 1);
        b.splice(at..at, rows(seed + 1, edit as u64, 97));
        let mut b2 = b.clone();
        b2.extend_from_slice(&rows(seed + 2, tail, modulo));
        let budget = Budget::new(Bytes(mem));
        let Ok(old) = Diff::new(Arc::clone(&a), Arc::<[u8]>::from(b.as_slice()), &budget, 2) else {
            return Ok(());
        };
        let (segments, starts) = (old.segments().to_vec(), old.starts().to_vec());
        drop(old);
        let prev = Previous {
            segments: &segments,
            starts: &starts,
            len_a: rift_core::Source::len(&*a),
            len_b: Bytes(b.len() as u64),
        };
        let b2: Arc<[u8]> = b2.into();
        let base = alloc::reset_peak().0;
        let guide = Guide::new(&*a, &*b2, &prev, &budget).unwrap();
        let run = incremental::diff(a, b2, guide, &budget, 2, Config::default(), &Watch::new());
        if let Ok((d, _)) = run {
            d.write_unified(&opts(), std::io::sink()).unwrap();
        }
        let used = alloc::peak().0 - base;
        prop_assert!(used <= mem, "peak {used} over {mem}");
    }
}
