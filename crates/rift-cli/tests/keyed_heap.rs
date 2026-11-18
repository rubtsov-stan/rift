//! Peak heap of key mode stays under `--mem`, however many passes the
//! budget forces.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters.

use rift_core::alloc::{self, StrictAlloc};
use rift_core::keyed::compare;
use rift_core::{Budget, Bytes};
use rift_gen::{Config, Profile, Rows};

#[global_allocator]
static ALLOC: StrictAlloc = StrictAlloc;

#[test]
fn key_mode_peaks_under_the_budget() {
    let mut config = Config::new(7, 16 << 20, Profile::Shuffled);
    config.rows = Rows::Csv;
    let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
    rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
    let spec = "csv:1".parse().unwrap();
    for mem in [Bytes::mib(1), Bytes::mib(4), Bytes::mib(64)] {
        let budget: &'static Budget = Box::leak(Box::new(Budget::new(mem)));
        alloc::watch(budget);
        let before = alloc::current().0;
        alloc::reset_peak();
        let mut changes = 0u64;
        let report = compare(&a[..], &b[..], &spec, budget, |_| {
            changes += 1;
            Ok(())
        })
        .unwrap();
        // Before anything here allocates: the budget holds nothing now.
        let excess = alloc::excess();
        let peak = alloc::peak().0 - before;
        eprintln!("--mem {mem}: peak {peak}, {report:?}");
        assert!(changes > 100);
        assert!(peak <= mem.0, "peak {peak} over --mem {mem}");
        assert!(
            excess <= 0,
            "--mem {mem}: {excess} bytes beyond the reservations"
        );
        if mem <= Bytes::mib(1) {
            assert!(report.passes > 1, "{report:?}");
        }
    }
}
