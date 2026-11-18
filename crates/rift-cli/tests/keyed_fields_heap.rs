//! Peak heap of key mode with field output stays under `--mem`.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters.

use rift_core::alloc::{self, CountingAlloc};
use rift_core::keyed::compare_fields;
use rift_core::{Budget, Bytes};
use rift_gen::{Config, Profile, Rows};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[test]
fn show_fields_peaks_under_the_budget() {
    let mut config = Config::new(7, 4 << 20, Profile::Shuffled);
    config.rows = Rows::Csv;
    config.change_ratio = 0.01;
    let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
    rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
    let spec = "csv:id".parse().unwrap();
    for mem in [Bytes::mib(1), Bytes::mib(4), Bytes::mib(64)] {
        let budget = Budget::new(mem);
        let before = alloc::current().0;
        alloc::reset_peak();
        let mut fields = 0u64;
        let report = compare_fields(&a[..], &b[..], &spec, None, &budget, |c| {
            fields += c.fields().map_or(0, |f| f.count() as u64);
            Ok(())
        })
        .unwrap();
        let peak = alloc::peak().0 - before;
        eprintln!("--mem {mem}: peak {peak}, {fields} fields, {report:?}");
        assert!(fields > 1000, "{fields}");
        assert!(peak <= mem.0, "peak {peak} over --mem {mem}");
    }
}
