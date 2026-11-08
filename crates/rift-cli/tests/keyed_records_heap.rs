//! Peak heap of key mode with the records read back stays under `--mem`.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters.

use std::io::Write;

use rift_core::alloc::{self, CountingAlloc};
use rift_core::keyed::{RecordReader, compare_with_ranges};
use rift_core::{Budget, Bytes};
use rift_gen::{Config, Profile, Rows};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[test]
fn show_records_peaks_under_the_budget() {
    let mut config = Config::new(7, 4 << 20, Profile::Shuffled);
    config.rows = Rows::Csv;
    config.change_ratio = 0.01;
    let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
    rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
    let spec = "csv:1".parse().unwrap();
    for mem in [Bytes::mib(1), Bytes::mib(4), Bytes::mib(64)] {
        let budget = Budget::new(mem);
        let before = alloc::current().0;
        alloc::reset_peak();
        let mut reader = RecordReader::new(&budget, 64 << 10).unwrap();
        let (mut records, mut bytes) = (0u64, 0u64);
        let report = compare_with_ranges(&a[..], &b[..], &spec, &budget, |c| {
            for (src, range) in [(&a, &c.a_range), (&b, &c.b_range)] {
                let Some(range) = range else { continue };
                records += 1;
                reader.read(&src[..], range.clone(), |chunk| {
                    bytes += chunk.len() as u64;
                    std::io::sink().write_all(chunk)
                })?;
            }
            Ok(())
        })
        .unwrap();
        drop(reader);
        let peak = alloc::peak().0 - before;
        eprintln!("--mem {mem}: peak {peak}, {records} records, {bytes} bytes, {report:?}");
        assert!(records > 1000, "{records}");
        assert!(peak <= mem.0, "peak {peak} over --mem {mem}");
    }
}
