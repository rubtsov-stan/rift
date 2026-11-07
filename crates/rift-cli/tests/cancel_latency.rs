//! How much a scan reads, and how long it runs, after it is cancelled, on a
//! large generated pair. Prints the figures; fails if a cancelled scan
//! reads more than one chunk per scan thread in the first pass, or one
//! comparison buffer per lane in the second.
//!
//! Needs a pair from `bench/data.sh` and a release build:
//!
//! ```sh
//! cargo test -p rift-diff --release --test cancel_latency -- --ignored --nocapture
//! ```
//!
//! `RIFT_PAIR` names the directory holding `a` and `b`, by default
//! `~/rift-data/gen10g`.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use rift_core::diff::{Config, Diff};
use rift_core::scan::Watch;
use rift_core::{Budget, ByteOffset, Bytes, Error, Source};

struct Counted {
    file: File,
    len: u64,
    read: Arc<AtomicU64>,
}

impl Source for Counted {
    fn len(&self) -> Bytes {
        Bytes(self.len)
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize> {
        let n = self.file.read_at(buf, offset.0)?;
        self.read.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

fn pair() -> (PathBuf, PathBuf) {
    let dir = std::env::var_os("RIFT_PAIR").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").unwrap()).join("rift-data/gen10g"),
        PathBuf::from,
    );
    (dir.join("a"), dir.join("b"))
}

/// Cancels a scan once `at` returns true of the bytes it has covered and
/// the size of both files; returns the bytes read after the cancel and how
/// long the scan ran on.
fn cancel_when(at: impl Fn(u64, u64) -> bool) -> (u64, Duration) {
    let (a, b) = pair();
    let read = Arc::new(AtomicU64::new(0));
    let open = |p: &PathBuf| {
        let file = File::open(p).unwrap();
        let len = file.metadata().unwrap().len();
        Arc::new(Counted {
            file,
            len,
            read: Arc::clone(&read),
        })
    };
    let (a, b) = (open(&a), open(&b));
    let both = a.len + b.len;
    let watch = Arc::new(Watch::new());
    let scan = {
        let watch = Arc::clone(&watch);
        std::thread::spawn(move || {
            let budget = Budget::new(Bytes::mib(256));
            Diff::with_watch(a, b, &budget, 4, Config::default(), &watch).map(|_| ())
        })
    };
    while !at(watch.done().0, both) {
        assert!(!scan.is_finished(), "the scan ended first");
        std::thread::sleep(Duration::from_millis(1));
    }
    let at_cancel = read.load(Ordering::Relaxed);
    let cancelled = Instant::now();
    watch.cancel();
    let result = scan.join().unwrap();
    let took = cancelled.elapsed();
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    (read.load(Ordering::Relaxed) - at_cancel, took)
}

#[test]
#[ignore = "needs a pair from bench/data.sh; run in release"]
fn a_cancelled_scan_stops_within_a_chunk_per_thread() {
    let threads = std::thread::available_parallelism().map_or(1, usize::from) as u64;
    for (what, at, limit) in [
        (
            "first pass, a quarter in",
            Box::new(|done: u64, both: u64| done > both / 4) as Box<dyn Fn(u64, u64) -> bool>,
            threads * ((16 << 20) + (64 << 10)),
        ),
        (
            "second pass, a quarter in",
            Box::new(|done: u64, both: u64| done > both + both / 4),
            threads * 2 * (1 << 20),
        ),
    ] {
        let (after, took) = cancel_when(at);
        eprintln!(
            "{what}: {after} bytes ({} KiB) read after cancel, stopped in {took:?}",
            after >> 10
        );
        assert!(after <= limit, "{after} bytes after cancel, limit {limit}");
    }
}
