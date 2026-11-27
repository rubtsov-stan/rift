//! Saving a diff keeps the whole run within its budget: the writer's
//! buffer comes from the Refine half before the diff takes the rest, the
//! block index from the Skeleton half, and the chunk tables go straight
//! to the sink.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::FileExt;
use std::sync::Arc;

use rift_core::alloc::{self, CountingAlloc};
use rift_core::diff::{Config, Diff};
use rift_core::saved::{Identity, Sink, Writer, write_diff};
use rift_core::unified::{Label, Options};
use rift_core::{Budget, ByteOffset, Bytes, Source};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

struct FileSink(File);

impl Source for FileSink {
    fn len(&self) -> Bytes {
        Bytes(self.0.metadata().unwrap().len())
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize> {
        self.0.read_at(buf, offset.0)
    }
}

impl Sink for FileSink {
    fn write_at(&self, buf: &[u8], offset: ByteOffset) -> io::Result<()> {
        self.0.write_all_at(buf, offset.0)
    }
}

#[test]
fn saving_a_diff_with_many_changes_stays_within_the_budget() {
    // 4 MiB per side, one changed line in 50: thousands of segments and
    // dozens of blocks.
    let n = 200_000;
    let row = |i: usize| format!("row {i:07} {}\n", i * 31).into_bytes();
    let a: Vec<u8> = (0..n).flat_map(row).collect();
    let b: Vec<u8> = (0..n)
        .flat_map(|i| {
            if i % 50 == 7 {
                format!("row {i:07} changed\n").into_bytes()
            } else {
                row(i)
            }
        })
        .collect();
    let (sa, sb): (Arc<[u8]>, Arc<[u8]>) = (a.into(), b.into());
    let path = std::env::temp_dir().join(format!("rift-save-heap-{}", std::process::id()));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&path)
        .unwrap();
    let sink = FileSink(file);
    let label = |name: &'static str| Label {
        name: name.as_bytes(),
        mtime: None,
    };
    let opts = Options {
        context: 3,
        old: label("a"),
        new: label("b"),
    };
    let mem = Bytes::mib(24);
    let base = alloc::reset_peak().0;
    {
        let budget = Budget::new(mem);
        let writer = Writer::new(&sink, [Source::len(&*sa), Source::len(&*sb)], &budget).unwrap();
        let tables = writer.chunk_tables();
        let diff = Diff::with_chunk_tables(
            Arc::clone(&sa),
            Arc::clone(&sb),
            &budget,
            2,
            Config::default(),
            tables,
        )
        .unwrap();
        let ids = [Identity::default(), Identity::default()];
        write_diff(&diff, writer, ids, mem, &opts, io::sink()).unwrap();
    }
    let peak = alloc::peak().0 - base;
    std::fs::remove_file(&path).unwrap();
    eprintln!("peak heap {peak} bytes under --mem {}", mem.0);
    // Thread names and the scheduler's per-thread bookkeeping are not
    // charged; they take a few KiB.
    assert!(
        peak <= mem.0 + (64 << 10),
        "peak heap {peak} bytes exceeds the {} byte budget",
        mem.0
    );
}
