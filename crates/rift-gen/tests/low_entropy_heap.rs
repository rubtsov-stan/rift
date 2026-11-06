//! Peak heap of the full diff of a small pair follows its line count, not
//! `--mem`.
//!
//! Alone in its binary: the allocator counters are process-wide.

use std::io;
use std::sync::Arc;

use rift_core::alloc::{self, CountingAlloc};
use rift_core::diff::Diff;
use rift_core::unified::{Label, Options};
use rift_core::{Budget, Bytes};
use rift_gen::{Config, Profile};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Fixed costs: the unified writer's 1 MiB buffer, line readers and
/// worker buffers. The 256 KiB `sql` pair below peaks at 1.2 MB in all.
const FLOOR: u64 = 4 << 20;
/// Bytes per line of A plus B. On an all-sampled pair the skeleton's
/// samples (24 bytes each) and anchor table (about 16 bytes per sample)
/// peak at 45.0 to 45.9 bytes per line, measured on `empty`, `distinct`
/// k=1 and `alternating` pairs of 256 KiB to 4 MiB.
const PER_LINE: u64 = 48;

/// Keeps nothing.
struct Sink;

impl io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn pair(profile: Profile) -> (Arc<[u8]>, Arc<[u8]>) {
    let mut config = Config::new(7, 256 << 10, profile);
    config.distinct = 1;
    let (mut a, mut b, mut m) = (Vec::new(), Vec::new(), Vec::new());
    rift_gen::generate(&config, &mut a, &mut b, &mut m).unwrap();
    (a.into(), b.into())
}

/// Peak heap of the diff of `a` and `b` under `mem`, unified output
/// included, above what was allocated when it started.
fn peak(a: &Arc<[u8]>, b: &Arc<[u8]>, mem: Bytes) -> u64 {
    let label = Label {
        name: b"x",
        mtime: None,
    };
    let opts = Options {
        context: 3,
        old: label,
        new: label,
    };
    let base = alloc::current().0;
    alloc::reset_peak();
    let diff = Diff::new(Arc::clone(a), Arc::clone(b), &Budget::new(mem), 4).unwrap();
    diff.write_unified(&opts, Sink).unwrap();
    drop(diff);
    alloc::peak().0 - base
}

#[test]
fn small_pairs_take_heap_by_line_count_under_any_budget() {
    for profile in [Profile::Empty, Profile::Distinct, Profile::Sql] {
        let (a, b) = pair(profile);
        let lines = a.iter().chain(b.iter()).filter(|&&c| c == b'\n').count() as u64;
        let bound = FLOOR + PER_LINE * lines;
        for mem in [Bytes::mib(256), Bytes::mib(1024)] {
            // An allocation sized from the budget would break the bound.
            assert!(
                bound < mem.0 / 8,
                "{profile:?}: bound {bound} too close to {mem}"
            );
            let p = peak(&a, &b, mem);
            println!("{profile:?}: {lines} lines, peak {p} of bound {bound} under {mem}");
            assert!(p <= bound, "{profile:?}: peak {p} over {bound} under {mem}");
        }
    }
}
