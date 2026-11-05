//! Peak heap of scan plus skeleton on a 1 GiB generated pair stays under
//! `--mem`.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters. Needs the pair from `bench/data.sh` and a release build:
//!
//! ```sh
//! cargo test -p rift-diff --release --test skeleton_heap -- --ignored
//! ```

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::PathBuf;

use rift_core::alloc::{self, CountingAlloc};
use rift_core::skeleton::build_pair;
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

fn open(path: &PathBuf) -> FileSource {
    let file = File::open(path).unwrap();
    let len = file.metadata().unwrap().len();
    FileSource(file, len)
}

#[test]
#[ignore = "needs ~/rift-data/gen1g from bench/data.sh; run in release"]
fn a_1gib_pair_maps_under_256mib_of_heap() {
    let dir = PathBuf::from(std::env::var_os("HOME").unwrap()).join("rift-data/gen1g");
    assert!(
        dir.join("b").exists(),
        "{} not found: generate it with bench/data.sh",
        dir.display()
    );
    let (a, b) = (open(&dir.join("a")), open(&dir.join("b")));
    let mem = Bytes::mib(256);
    alloc::reset_peak();
    let (_, _, map) = build_pair(&a, &b, &Budget::new(mem)).unwrap();
    let peak = alloc::peak();
    eprintln!(
        "peak {} MiB, {} segments, {} anchors, stratum {}",
        peak.0 >> 20,
        map.segments().len(),
        map.anchors(),
        map.level()
    );
    assert!(!map.is_identical());
    assert!(peak <= mem, "peak {peak} over {mem}");
}
