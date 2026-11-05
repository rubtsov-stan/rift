//! Command-line entry point for Rift.
#![deny(unsafe_code)]

mod source;
mod summary;

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::Parser;
use rift_core::Source;
use rift_core::alloc::CountingAlloc;

use crate::source::FileSource;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Diff text files of any size under a hard memory limit.
#[derive(Debug, Parser)]
#[command(name = "rift", version)]
struct Args {
    /// First file.
    a: PathBuf,
    /// Second file.
    b: PathBuf,
    /// Print line counts, the segment map and resource use instead of a
    /// diff.
    #[arg(long)]
    summary: bool,
    /// Heap limit, in bytes or with a K, M, G or T unit (powers of 1024).
    #[arg(long, default_value = "256M", value_parser = rift_core::Bytes::parse_budget)]
    mem: rift_core::Bytes,
}

fn run(args: &Args) -> anyhow::Result<ExitCode> {
    let a = FileSource::open(&args.a).with_context(|| format!("open {}", args.a.display()))?;
    let b = FileSource::open(&args.b).with_context(|| format!("open {}", args.b.display()))?;
    if args.summary {
        return summary::run(&a, &b, args.mem);
    }
    println!("{} bytes, {} bytes", a.len(), b.len());
    Ok(ExitCode::SUCCESS)
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("rift: {err:#}");
            ExitCode::from(2)
        }
    }
}
