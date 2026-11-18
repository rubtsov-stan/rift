//! While key mode runs, the heap never holds more than the budget has
//! reserved, for a key named by a CSV header, an NDJSON path and a `COPY`
//! column, with and without field output, and when runs follow each other
//! in one process as they do under `rift serve` and the MCP server.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters.

use rift_core::alloc::{self, StrictAlloc};
use rift_core::keyed::{compare, compare_fields};
use rift_core::{Budget, Bytes};
use rift_gen::{Config, Profile, Rows};

#[global_allocator]
static ALLOC: StrictAlloc = StrictAlloc;

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
        // 64M runs a pool of threads; the runs after it, with no pause,
        // see whatever its threads leave behind.
        for fields in [false, true] {
            for mem in [Bytes::mib(64), Bytes::mib(1), Bytes::mib(4)] {
                let budget: &'static Budget = Box::leak(Box::new(Budget::new(mem)));
                alloc::watch(budget);
                if fields {
                    compare_fields(&a[..], &b[..], &spec, None, budget, |_| Ok(())).unwrap();
                } else {
                    compare(&a[..], &b[..], &spec, budget, |_| Ok(())).unwrap();
                }
                let excess = alloc::excess();
                assert!(
                    excess <= 0,
                    "{spec:?}, fields {fields}, --mem {mem}: {excess} bytes beyond the reservations"
                );
            }
        }
    }
}
