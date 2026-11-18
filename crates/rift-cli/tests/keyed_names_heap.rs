//! While key mode runs, the heap it adds stays within what the budget has
//! reserved, for a key named by a CSV header, an NDJSON path and a `COPY`
//! column.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters.

use rift_core::alloc::{self, CountingAlloc};
use rift_core::keyed::compare;
use rift_core::{Budget, Bytes, Half};
use rift_gen::{Config, Profile, Rows};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[test]
fn named_keys_allocate_only_what_the_budget_holds() {
    for (rows, spec) in [
        (Rows::Csv, "csv:id"),
        (Rows::Ndjson, "json:id"),
        (Rows::Sql, "copy:id"),
    ] {
        let mut config = Config::new(7, 4 << 20, Profile::Shuffled);
        config.rows = rows;
        config.change_ratio = 0.01;
        let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
        rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
        let spec = spec.parse().unwrap();
        let budget = Budget::new(Bytes::mib(1));
        let before = alloc::current().0;
        // Each change is reported with both readers alive; the heap added
        // since `before` beyond the reserved bytes is outside the budget.
        let mut worst = 0;
        compare(&a[..], &b[..], &spec, &budget, |_| {
            let reserved = budget.used(Half::Skeleton).0 + budget.used(Half::Refine).0;
            let added = alloc::current().0.cast_signed() - before.cast_signed();
            worst = worst.max(added - reserved.cast_signed());
            Ok(())
        })
        .unwrap();
        assert!(
            worst <= 0,
            "{spec:?}: {worst} bytes beyond the reservations"
        );
    }
}
