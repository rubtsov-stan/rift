//! Times a fresh diff of A and B' against one guided by the map of A and
//! B, interleaved, and checks that both print the same unified diff.
//!
//! ```sh
//! cargo run --release -p rift-diff-core --example incremental -- A B B2 [MEM_MIB] [RUNS]
//! ```
//!
//! Prints one line per run (`fresh` or `guided`, seconds, peak heap
//! bytes) and the guided run's path. The map of A and B is built once,
//! before the timed runs.

use std::fs::File;
use std::io::{self, BufWriter};
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::time::Instant;

use rift_core::alloc::{self, CountingAlloc};
use rift_core::diff::{Config, Diff};
use rift_core::incremental::{self, Guide, Previous};
use rift_core::scan::Watch;
use rift_core::unified::{Label, Options};
use rift_core::{Budget, ByteOffset, Bytes, DiffView, Source};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

#[derive(Debug)]
struct Pread(File, u64);

impl Source for Pread {
    fn len(&self) -> Bytes {
        Bytes(self.1)
    }

    fn read_at(&self, buf: &mut [u8], at: ByteOffset) -> io::Result<usize> {
        self.0.read_at(buf, at.0)
    }
}

fn open(path: &str) -> io::Result<Arc<Pread>> {
    let f = File::open(path)?;
    let len = f.metadata()?.len();
    Ok(Arc::new(Pread(f, len)))
}

fn opts() -> Options<'static> {
    let label = |name: &'static str| Label {
        name: name.as_bytes(),
        mtime: None,
    };
    Options {
        context: 3,
        old: label("a"),
        new: label("b"),
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let [_, a, b, b2, rest @ ..] = args.as_slice() else {
        return Err("usage: incremental A B B2 [MEM_MIB] [RUNS]".into());
    };
    let mem = Bytes::mib(rest.first().map_or(Ok(256), |m| m.parse())?);
    let runs: usize = rest.get(1).map_or(Ok(10), |r| r.parse())?;
    let threads = rift_core::budget::threads(mem).min(rift_core::budget::cores());
    let (a, b, b2) = (open(a)?, open(b)?, open(b2)?);
    let budget = Budget::new(mem);
    let old = Diff::new(Arc::clone(&a), Arc::clone(&b), &budget, threads)?;
    let (segments, starts) = (old.segments().to_vec(), old.starts().to_vec());
    drop(old);
    let prev = Previous {
        segments: &segments,
        starts: &starts,
        len_a: a.len(),
        len_b: b.len(),
    };
    let out = std::env::temp_dir();
    let (out_fresh, out_guided) = (
        out.join("rift-incr-fresh.diff"),
        out.join("rift-incr-guided.diff"),
    );
    for _ in 0..runs {
        for guided in [false, true] {
            let base = alloc::reset_peak().0;
            let start = Instant::now();
            let path = if guided {
                let guide = Guide::new(&*a, &*b2, &prev, &budget)?;
                let (d, path) = incremental::diff(
                    Arc::clone(&a),
                    Arc::clone(&b2),
                    guide,
                    &budget,
                    threads,
                    Config::default(),
                    &Watch::new(),
                )?;
                d.write_unified(&opts(), BufWriter::new(File::create(&out_guided)?))?;
                Some(path)
            } else {
                let d = Diff::new(Arc::clone(&a), Arc::clone(&b2), &budget, threads)?;
                d.write_unified(&opts(), BufWriter::new(File::create(&out_fresh)?))?;
                None
            };
            let secs = start.elapsed().as_secs_f64();
            let peak = alloc::peak().0 - base;
            let name = if guided { "guided" } else { "fresh" };
            println!("{name} {secs:.3} {peak} {path:?}");
        }
        if std::fs::read(&out_fresh)? != std::fs::read(&out_guided)? {
            return Err("outputs differ".into());
        }
    }
    println!("identical");
    Ok(())
}
