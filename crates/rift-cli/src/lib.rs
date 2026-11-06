//! Command-line definition of Rift, shared with the man page generator.

use std::path::PathBuf;

use clap::Parser;

/// Diff text files of any size under a hard memory limit.
#[derive(Debug, Parser)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one bool per command-line flag"
)]
#[command(name = "rift", version)]
pub struct Args {
    /// First file.
    pub a: PathBuf,
    /// Second file.
    pub b: PathBuf,
    /// Print line counts, the segment map and resource use instead of a
    /// diff.
    #[arg(long)]
    pub summary: bool,
    /// Heap limit, in bytes or with a K, M, G or T unit (powers of 1024).
    #[arg(long, default_value = "256M", value_parser = rift_core::Bytes::parse_budget)]
    pub mem: rift_core::Bytes,
    /// Browse the diff in the terminal instead of printing it.
    #[arg(short, long, conflicts_with = "summary")]
    pub interactive: bool,
    /// Skip the search for moved blocks. The diff is the same.
    #[arg(long, conflicts_with = "key")]
    pub no_moves: bool,
    /// After a diff, print peak heap, time and approximate regions to
    /// standard error.
    #[arg(long)]
    pub stats: bool,
    /// Compare records by key instead of lines, regardless of row order:
    /// csv:N (column, from 1), csv:NAME (header), json:PATH (dotted field)
    /// or copy:NAME (COPY column of a Postgres dump).
    #[arg(long, value_name = "SPEC", conflicts_with_all = ["summary", "interactive"])]
    pub key: Option<rift_core::keyed::KeySpec>,
}
