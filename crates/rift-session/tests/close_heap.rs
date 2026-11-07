//! Closing a scanning session stops its scan at once and gives its heap
//! back.
//!
//! Alone in its binary because it reads the process-wide allocator counters.

use std::io::Write;
use std::time::{Duration, Instant};

use rift_core::Bytes;
use rift_core::alloc::{self, CountingAlloc};
use rift_session::{Limits, SessionState, Sessions};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// About `mib` MiB of distinct numbered lines.
fn write_lines(path: &std::path::Path, mib: u64, tag: &str) {
    let mut out = std::io::BufWriter::new(std::fs::File::create(path).unwrap());
    let mut written = 0;
    let mut i = 0u64;
    while written < mib << 20 {
        let line = format!("{tag} {i:012} some text to make the line longer\n");
        out.write_all(line.as_bytes()).unwrap();
        written += line.len() as u64;
        i += 1;
    }
    out.flush().unwrap();
}

#[test]
fn closing_a_scan_stops_it_and_releases_its_heap() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = (dir.path().join("a"), dir.path().join("b"));
    write_lines(&a, 48, "old");
    write_lines(&b, 48, "new");
    let s = Sessions::new(Limits::default());
    let before = alloc::current().0;
    let session = s
        .open(
            a.to_str().unwrap(),
            b.to_str().unwrap(),
            Some(Bytes::mib(64)),
        )
        .unwrap()
        .session;
    // Wait until the scan holds samples: a chunk of A is done.
    let began = Instant::now();
    while session.scan_progress().0 == Bytes(0) {
        assert!(
            began.elapsed() < Duration::from_secs(20),
            "the scan never started"
        );
        std::thread::yield_now();
    }
    let during = alloc::current().0 - before;
    let (at_close, total) = session.scan_progress();
    let closed = Instant::now();
    assert!(s.close(session.id()));
    assert_eq!(session.wait(), SessionState::Failed, "the scan ended first");
    let stopped = closed.elapsed();
    let (at_stop, _) = session.scan_progress();
    let after = alloc::current().0.saturating_sub(before);
    eprintln!(
        "heap during scan {during} B, after close {after} B; stopped in {stopped:?}, \
         {} bytes counted after close of {}",
        at_stop.0 - at_close.0,
        total.0
    );
    assert!(during > 1 << 20, "the scan held {during} bytes");
    assert!(
        after < 4 << 10,
        "{after} bytes still held after the scan stopped"
    );
    assert!(stopped < Duration::from_secs(1), "{stopped:?}");
    assert!(at_stop < total);
    assert_eq!(s.mem(), Bytes(0));
}
