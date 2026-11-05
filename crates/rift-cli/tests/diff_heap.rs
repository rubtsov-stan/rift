//! Peak heap of the full diff of a 1 GiB generated pair, unified output
//! included, stays under `--mem`.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters. Needs the pair from `bench/data.sh` and a release build:
//!
//! ```sh
//! cargo test -p rift-diff --release --test diff_heap -- --ignored
//! ```

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;
use std::sync::Arc;

use rift_core::alloc::{self, CountingAlloc};
use rift_core::diff::Diff;
use rift_core::unified::{Label, Options, Verdict};
use rift_core::{Budget, ByteOffset, Bytes, Source};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

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

#[test]
#[ignore = "needs ~/rift-data/gen1g from bench/data.sh; run in release"]
fn a_1gib_pair_diffs_under_256mib_of_heap() {
    let dir = PathBuf::from(std::env::var_os("HOME").unwrap()).join("rift-data/gen1g");
    assert!(
        dir.join("b").exists(),
        "{} not found: generate it with bench/data.sh",
        dir.display()
    );
    let (a, b) = (open(&dir.join("a")), open(&dir.join("b")));
    let mem = Bytes::mib(256);
    alloc::reset_peak();
    let threads = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let diff = Diff::new(a, b, &Budget::new(mem), threads).unwrap();
    let label = Label {
        name: b"a",
        mtime: None,
    };
    let opts = Options {
        context: 3,
        old: label,
        new: label,
    };
    let mut out = Count(0);
    let written = diff.write_unified(&opts, &mut out).unwrap();
    drop(diff);
    let peak = alloc::peak();
    eprintln!(
        "peak {} MiB, {} bytes of diff, {} approx segments",
        peak.0 >> 20,
        out.0,
        written.approx_segments
    );
    assert_eq!(written.verdict, Verdict::Different);
    assert!(peak <= mem, "peak {peak} over {mem}");
}
