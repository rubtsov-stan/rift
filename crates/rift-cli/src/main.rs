//! Command-line entry point for Rift.
#![deny(unsafe_code)]

mod full;
mod source;
mod summary;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
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
    /// Browse the diff in the terminal instead of printing it.
    #[arg(short, long, conflicts_with = "summary")]
    interactive: bool,
    /// After a diff, print peak heap, time and approximate regions to
    /// standard error.
    #[arg(long)]
    stats: bool,
}

/// `rift tui A B` is `rift -i A B`. With fewer than two more arguments
/// `tui` is a file name: `rift tui other` diffs a file called `tui`.
fn tui_alias(mut args: Vec<std::ffi::OsString>) -> Vec<std::ffi::OsString> {
    if args.len() >= 4 && args[1] == "tui" {
        args[1] = "-i".into();
    }
    args
}

fn run(args: &Args) -> anyhow::Result<ExitCode> {
    let a = FileSource::open(&args.a).with_context(|| format!("open {}", args.a.display()))?;
    let b = FileSource::open(&args.b).with_context(|| format!("open {}", args.b.display()))?;
    if args.summary {
        return summary::run(&a, &b, args.mem);
    }
    let files = full::Files {
        a: Arc::new(a),
        b: Arc::new(b),
        path_a: &args.a,
        path_b: &args.b,
    };
    if args.interactive {
        full::interactive(&files, args.mem)
    } else {
        full::unified(&files, args.mem, args.stats)
    }
}

fn main() -> ExitCode {
    let args = Args::parse_from(tui_alias(std::env::args_os().collect()));
    match run(&args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("rift: {err:#}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alias(args: &[&str]) -> Vec<String> {
        tui_alias(args.iter().map(Into::into).collect())
            .into_iter()
            .map(|a| a.into_string().unwrap())
            .collect()
    }

    #[test]
    fn tui_is_interactive_unless_it_is_a_file() {
        assert_eq!(alias(&["rift", "tui", "a", "b"]), ["rift", "-i", "a", "b"]);
        assert_eq!(alias(&["rift", "tui", "other"]), ["rift", "tui", "other"]);
        assert_eq!(alias(&["rift", "a", "tui", "b"]), ["rift", "a", "tui", "b"]);
        let args = Args::parse_from(alias(&["rift", "tui", "--mem", "1G", "a", "b"]));
        assert!(args.interactive);
    }
}
