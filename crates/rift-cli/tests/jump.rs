//! Time from a jump to a ready hunk, as the terminal interface sees it.
//!
//! Needs a pair from `bench/data.sh` and a release build; `RIFT_PAIR`
//! names the directory, `~/rift-data/gen10g` by default:
//!
//! ```sh
//! cargo test -p rift-diff --release --test jump -- --ignored --nocapture
//! ```

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rift_core::diff::Diff;
use rift_core::{Budget, ByteOffset, Bytes, DiffView, HunkState, LineNo, SegmentStatus, Source};

struct FileSource(File, u64);

impl Source for FileSource {
    fn len(&self) -> Bytes {
        Bytes(self.1)
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize> {
        self.0.read_at(buf, offset.0)
    }
}

fn open(path: &PathBuf) -> Arc<FileSource> {
    let file = File::open(path).unwrap();
    let len = file.metadata().unwrap().len();
    Arc::new(FileSource(file, len))
}

#[test]
#[ignore = "needs a pair from bench/data.sh; run in release"]
fn a_jump_to_a_differing_segment_is_ready_within_100ms() {
    let dir = std::env::var_os("RIFT_PAIR").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").unwrap()).join("rift-data/gen10g"),
        PathBuf::from,
    );
    let (a, b) = (open(&dir.join("a")), open(&dir.join("b")));
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let start = Instant::now();
    let diff = Diff::new(a, b, &Budget::new(Bytes::mib(256)), threads).unwrap();
    let map = start.elapsed();
    let segs = diff.segments();
    let differing: Vec<usize> = (0..segs.len())
        .filter(|&i| segs[i].status != SegmentStatus::Equal)
        .collect();
    // Jumps spread over the file, far from each other and from where
    // background work has got to.
    let jumps = 200;
    let mut times: Vec<Duration> = (0..jumps)
        .map(|k| {
            let i = differing[(differing.len() - 1) - k * (differing.len() / jumps)];
            let s = &segs[i];
            let t0 = Instant::now();
            diff.set_visible(s.a.start..LineNo(s.a.end.0.max(s.a.start.0 + 1)));
            loop {
                match diff.hunk(i) {
                    HunkState::Pending => std::thread::sleep(Duration::from_micros(200)),
                    _ => break t0.elapsed(),
                }
            }
        })
        .collect();
    times.sort_unstable();
    let at = |p: usize| times[(times.len() * p).div_ceil(100).max(1) - 1];
    println!(
        "map in {:.1} s; {} jumps: p50 {:?}, p99 {:?}, max {:?}",
        map.as_secs_f64(),
        times.len(),
        at(50),
        at(99),
        at(100)
    );
    assert!(at(99) <= Duration::from_millis(100), "p99 {:?}", at(99));
}
