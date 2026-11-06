//! The full pipeline under `--mem` on a 50 GB pair that is never written
//! to disk.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters. The run is ignored by default:
//!
//! ```sh
//! cargo test -p rift-gen --release --test synthetic -- --ignored --nocapture
//! ```

use std::io;
use std::sync::Arc;
use std::time::Instant;

use rift_core::alloc::{self, CountingAlloc};
use rift_core::diff::Diff;
use rift_core::skeleton::build_pair;
use rift_core::unified::{Label, Options, Verdict};
use rift_core::{Budget, Bytes};
use rift_gen::synthetic::{Side, Synthetic};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

const SEED: u64 = 7;
const CHANGE: f64 = 0.001;

fn pair(size: u64) -> (Arc<Synthetic>, Arc<Synthetic>) {
    (
        Arc::new(Synthetic::new(SEED, size, CHANGE, Side::A)),
        Arc::new(Synthetic::new(SEED, size, CHANGE, Side::B)),
    )
}

/// Counts what it is given and keeps nothing.
struct Count(u64);

impl io::Write for Count {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0 += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn options() -> Options<'static> {
    let label = Label {
        name: b"synthetic",
        mtime: None,
    };
    Options {
        context: 3,
        old: label,
        new: label,
    }
}

/// Peak heap of `f`, measured from what is allocated when it starts.
fn peak_of<T>(f: impl FnOnce() -> T) -> (T, Bytes) {
    alloc::reset_peak();
    let base = alloc::current().0;
    let out = f();
    (out, Bytes(alloc::peak().0 - base))
}

#[allow(clippy::cast_precision_loss, reason = "display only")]
fn mib(b: Bytes) -> f64 {
    b.0 as f64 / f64::from(1 << 20)
}

#[test]
#[ignore = "diffs 2 x 50 GB generated on the fly; run in release, takes minutes"]
fn a_50gb_pair_stays_under_256mib() {
    let size = std::env::var("RIFT_SYNTH_BYTES").map_or(50_000_000_000, |s| s.parse().unwrap());
    let mem = Bytes::mib(256);
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let (a, b) = pair(size);

    let start = Instant::now();
    let (map, peak) = peak_of(|| {
        let (_, _, map) = build_pair(&*a, &*b, &Budget::new(mem)).unwrap();
        (map.segments().len(), map.level(), map.anchors())
    });
    println!(
        "summary: {} segments, stratum {}, {} anchors, peak heap {:.1} MiB of 256 MiB, {:.0} s",
        map.0,
        map.1,
        map.2,
        mib(peak),
        start.elapsed().as_secs_f64()
    );
    assert!(peak <= mem, "summary peak {peak} over {mem}");

    let start = Instant::now();
    let ((written, bytes), peak) = peak_of(|| {
        let diff = Diff::new(Arc::clone(&a), Arc::clone(&b), &Budget::new(mem), threads).unwrap();
        let mut out = Count(0);
        let written = diff.write_unified(&options(), &mut out).unwrap();
        (written, out.0)
    });
    println!(
        "full diff: {bytes} bytes of diff, {} approx segments, peak heap {:.1} MiB of 256 MiB, {:.0} s",
        written.approx_segments,
        mib(peak),
        start.elapsed().as_secs_f64()
    );
    println!(
        "source bookkeeping: {} bytes per file, no heap",
        size_of::<Synthetic>()
    );
    assert_eq!(written.verdict, Verdict::Different);
    assert!(peak <= mem, "full diff peak {peak} over {mem}");
}
