//! A scan followed and stopped from outside through a [`Watch`].

use std::io;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use rift_core::diff::{Config, Diff};
use rift_core::scan::Watch;
use rift_core::skeleton::build_pair_watched;
use rift_core::{Budget, ByteOffset, Bytes, Error, Source};

/// An in-memory file that counts what it serves, records the watch's
/// progress at every read, and cancels the watch once `cancel_at` bytes
/// have been read.
struct Probe<'w> {
    data: Vec<u8>,
    watch: &'w Watch,
    read: &'w AtomicU64,
    seen: &'w Mutex<Vec<u64>>,
    cancel_at: u64,
}

impl Source for Probe<'_> {
    fn len(&self) -> Bytes {
        Bytes(self.data.len() as u64)
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize> {
        let n = self.data.as_slice().read_at(buf, offset)?;
        self.seen.lock().unwrap().push(self.watch.done().0);
        if self.read.fetch_add(n as u64, Ordering::Relaxed) + n as u64 >= self.cancel_at {
            self.watch.cancel();
        }
        Ok(n)
    }
}

fn lines(tag: &str, n: usize) -> Vec<u8> {
    (0..n)
        .flat_map(|i| format!("{tag} row {i:08} with some padding text\n").into_bytes())
        .collect()
}

fn pair(n: usize) -> (Vec<u8>, Vec<u8>) {
    let a = lines("same", n);
    let mut b = a.clone();
    let mid = b.len() / 2;
    b.splice(mid..mid, b"an inserted line\n".iter().copied());
    (a, b)
}

#[test]
fn progress_grows_to_twice_both_sizes() {
    let (a, b) = pair(200_000);
    let total = 2 * (a.len() + b.len()) as u64;
    let watch = Watch::new();
    let (read, seen) = (AtomicU64::new(0), Mutex::new(Vec::new()));
    let probe = |data| Probe {
        data,
        watch: &watch,
        read: &read,
        seen: &seen,
        cancel_at: u64::MAX,
    };
    let (a, b) = (probe(a), probe(b));
    build_pair_watched(&a, &b, &Budget::new(Bytes::mib(64)), &watch).unwrap();
    let seen = seen.into_inner().unwrap();
    assert!(seen.len() > 4, "{} reads", seen.len());
    assert!(seen.windows(2).all(|w| w[0] <= w[1]), "progress went back");
    assert!(seen.iter().any(|&d| d > 0 && d < total), "no step between");
    assert_eq!(watch.done().0, total);
}

#[test]
fn a_cancelled_scan_stops_within_a_few_chunks() {
    // 32 MiB per file; a 64 MiB budget scans in 4 MiB chunks on at most
    // two threads.
    let (a, b) = pair(800_000);
    let watch = Watch::new();
    let (read, seen) = (AtomicU64::new(0), Mutex::new(Vec::new()));
    let cancel_at = 8 << 20;
    let probe = |data| Probe {
        data,
        watch: &watch,
        read: &read,
        seen: &seen,
        cancel_at,
    };
    let (a, b) = (probe(a), probe(b));
    let err = build_pair_watched(&a, &b, &Budget::new(Bytes::mib(64)), &watch).unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    let after = read.load(Ordering::Relaxed) - cancel_at;
    assert!(
        after <= 2 * ((4 << 20) + (64 << 10)),
        "{after} bytes after cancel"
    );
}

#[test]
fn a_cancelled_comparison_stops_within_a_few_buffers() {
    // Identical files: the second pass streams one long equal gap.
    let a = lines("same", 800_000);
    let len = a.len() as u64;
    let watch = Watch::new();
    let (read, seen) = (AtomicU64::new(0), Mutex::new(Vec::new()));
    let cancel_at = 2 * len + (4 << 20);
    let probe = |data| Probe {
        data,
        watch: &watch,
        read: &read,
        seen: &seen,
        cancel_at,
    };
    let (a, b) = (probe(a.clone()), probe(a));
    let err = build_pair_watched(&a, &b, &Budget::new(Bytes::mib(64)), &watch).unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
    let after = read.load(Ordering::Relaxed) - cancel_at;
    let lanes = std::thread::available_parallelism().map_or(1, usize::from) as u64;
    assert!(after <= lanes * 2 * (1 << 20), "{after} bytes after cancel");
}

#[test]
fn a_diff_built_under_a_cancelled_watch_fails() {
    let (a, b) = pair(1000);
    let watch = Watch::new();
    watch.cancel();
    let err = Diff::with_watch(
        Arc::<[u8]>::from(a),
        Arc::<[u8]>::from(b),
        &Budget::new(Bytes::mib(16)),
        2,
        Config::default(),
        &watch,
    )
    .unwrap_err();
    assert!(matches!(err, Error::Cancelled), "{err}");
}

/// Like [`Probe`], owned so a [`Diff`] can hold it.
struct Trip {
    data: Vec<u8>,
    watch: Arc<Watch>,
    read: Arc<AtomicU64>,
    cancel_at: u64,
}

impl Source for Trip {
    fn len(&self) -> Bytes {
        Bytes(self.data.len() as u64)
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize> {
        let n = self.data.as_slice().read_at(buf, offset)?;
        if self.read.fetch_add(n as u64, Ordering::Relaxed) + n as u64 >= self.cancel_at {
            self.watch.cancel();
        }
        Ok(n)
    }
}

#[test]
fn a_watch_cancelled_as_the_scan_ends_stops_the_build_before_the_move_search() {
    let (a, b) = pair(20_000);
    let trips = |cancel_at| {
        let (watch, read) = (Arc::new(Watch::new()), Arc::new(AtomicU64::new(0)));
        let trip = |data: &Vec<u8>| {
            Arc::new(Trip {
                data: data.clone(),
                watch: Arc::clone(&watch),
                read: Arc::clone(&read),
                cancel_at,
            })
        };
        (trip(&a), trip(&b), watch, read)
    };
    // What the scan alone reads.
    let (ta, tb, watch, read) = trips(u64::MAX);
    build_pair_watched(&*ta, &*tb, &Budget::new(Bytes::mib(64)), &watch).unwrap();
    let scan_bytes = read.load(Ordering::Relaxed);
    // Cancelled by the scan's last read: nothing after it is read.
    let (ta, tb, watch, read) = trips(scan_bytes);
    let built = Diff::with_watch(
        ta,
        tb,
        &Budget::new(Bytes::mib(64)),
        2,
        Config::default(),
        &watch,
    );
    let Err(err) = built else {
        panic!("the build went on after the cancel");
    };
    assert!(matches!(err, Error::Cancelled), "{err}");
    assert_eq!(
        read.load(Ordering::Relaxed),
        scan_bytes,
        "read after the scan"
    );
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(64))]

    /// Whatever the pair, budget retries included, the count ends at twice
    /// both sizes.
    #[test]
    fn progress_ends_at_twice_both_sizes(
        a in proptest::collection::vec(0u8..4, 0..3000),
        b in proptest::collection::vec(0u8..4, 0..3000),
        kib in 64u64..2048,
    ) {
        // Few distinct short lines: many anchors and gaps of every shape.
        let text = |v: &[u8]| -> Vec<u8> {
            v.iter().map(|&c| if c == 0 { b'\n' } else { b'a' + c }).collect()
        };
        let (a, b) = (text(&a), text(&b));
        let watch = Watch::new();
        let budget = Budget::new(Bytes(kib << 10));
        if build_pair_watched(&a[..], &b[..], &budget, &watch).is_ok() {
            proptest::prop_assert_eq!(watch.done().0, 2 * (a.len() + b.len()) as u64);
        }
    }
}
