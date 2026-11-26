//! Opening a saved result and reading every script and every line keeps
//! within the budget given to the open.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters.

use std::io;
use std::sync::{Arc, Mutex};

use rift_core::alloc::{self, CountingAlloc};
use rift_core::diff::{Config, Diff};
use rift_core::saved::{Identity, SavedDiff, SavedFile, Sink, Writer, write_diff};
use rift_core::unified::{Label, Options};
use rift_core::{Budget, ByteOffset, Bytes, DiffView, LineAccess, LineBuf, LineNo, Source};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[derive(Default)]
struct MemSink(Mutex<Vec<u8>>);

impl Source for MemSink {
    fn len(&self) -> Bytes {
        Bytes(self.0.lock().unwrap().len() as u64)
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize> {
        self.0.lock().unwrap().as_slice().read_at(buf, offset)
    }
}

impl Sink for MemSink {
    fn write_at(&self, buf: &[u8], offset: ByteOffset) -> io::Result<()> {
        let mut v = self.0.lock().unwrap();
        let at = usize::try_from(offset.0).unwrap();
        if v.len() < at + buf.len() {
            v.resize(at + buf.len(), 0);
        }
        v[at..at + buf.len()].copy_from_slice(buf);
        Ok(())
    }
}

#[test]
fn opening_and_reading_a_result_stays_within_the_budget() {
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
    let sink = MemSink::default();
    {
        let budget = Budget::new(Bytes::mib(64));
        let sizes = [Bytes(a.len() as u64), Bytes(b.len() as u64)];
        let writer = Writer::new(&sink, sizes, &budget).unwrap();
        let tables = writer.chunk_tables();
        let (sa, sb): (Arc<[u8]>, Arc<[u8]>) = (a.as_slice().into(), b.as_slice().into());
        let diff = Diff::with_chunk_tables(sa, sb, &budget, 2, Config::default(), tables).unwrap();
        let label = |name: &'static str| Label {
            name: name.as_bytes(),
            mtime: None,
        };
        let opts = Options {
            context: 3,
            old: label("a"),
            new: label("b"),
        };
        let ids = [a.len(), b.len()].map(|size| Identity {
            size: size as u64,
            ..Identity::default()
        });
        write_diff(&diff, writer, ids, Bytes::mib(64), &opts, io::sink()).unwrap();
    }
    let result = sink.0.into_inner().unwrap();
    let mem = Bytes::mib(8);
    let mut buf = LineBuf::new();
    let base = alloc::reset_peak().0;
    {
        let budget = Budget::new(mem);
        let file = SavedFile::open(&result[..], &budget).unwrap();
        let view = SavedDiff::open(file, &a[..], &b[..], &budget).unwrap();
        let mut edits = 0;
        for i in 0..view.segments().len() {
            if let Some(h) = view.try_hunk(i).unwrap() {
                edits += h.edits.len();
            }
        }
        assert!(edits > 4000, "{edits} edits");
        let sides: [(&dyn LineAccess, u64); 2] = [
            (view.lines_a(), view.len_a()),
            (view.lines_b(), view.len_b()),
        ];
        for (side, n) in sides {
            let mut line = 0;
            while line < n {
                let end = (line + 64).min(n);
                side.read_lines(LineNo(line)..LineNo(end), &mut buf)
                    .unwrap();
                line = end;
            }
        }
    }
    let peak = alloc::peak().0 - base;
    eprintln!("peak heap {peak} bytes under --mem {}", mem.0);
    // The caller's line buffer is outside the budget, as in the terminal
    // interface; 64 lines of 20 bytes.
    assert!(
        peak <= mem.0 + (64 << 10),
        "peak heap {peak} bytes exceeds the {} byte budget",
        mem.0
    );
}
