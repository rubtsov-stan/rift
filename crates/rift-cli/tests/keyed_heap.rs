//! Peak heap of key mode stays under `--mem`, however many passes the
//! budget forces.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters.

use rift_core::alloc::{self, CountingAlloc};
use rift_core::keyed::compare;
use rift_core::{Budget, Bytes};
use rift_gen::{Config, Profile, Rows};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[test]
fn key_mode_peaks_under_the_budget() {
    let mut config = Config::new(7, 16 << 20, Profile::Shuffled);
    config.rows = Rows::Csv;
    let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
    rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
    let spec = "csv:1".parse().unwrap();
    for mem in [Bytes::mib(1), Bytes::mib(4), Bytes::mib(64)] {
        let budget = Budget::new(mem);
        let before = alloc::current().0;
        alloc::reset_peak();
        let mut changes = 0u64;
        let report = compare(&a[..], &b[..], &spec, &budget, |_| {
            changes += 1;
            Ok(())
        })
        .unwrap();
        let peak = alloc::peak().0 - before;
        eprintln!("--mem {mem}: peak {peak}, {report:?}");
        assert!(changes > 100);
        assert!(peak <= mem.0, "peak {peak} over --mem {mem}");
        if mem <= Bytes::mib(1) {
            assert!(report.passes > 1, "{report:?}");
        }
    }
}
