//! Command-line definition of Rift, shared with the man page generator.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use rift_core::Bytes;

/// The smallest `--mem`. Parsing the command line already takes about
/// 28 KiB of heap, before any budget exists, so a smaller limit could not
/// be kept.
pub const MIN_MEM: Bytes = Bytes(64 << 10);

/// The least `--mem` a full diff runs under, whatever the files hold: the refine half holds
/// the comparison buffers of one worker, about 5 MiB, and is half of
/// `--mem`. The help text and the man page give it as `10475K`; a test
/// keeps them equal. `--summary` and `--key` run below it.
pub const FULL_DIFF_MEM: Bytes = Bytes(10475 << 10);

/// Parses `--mem`: a size as [`Bytes::parse_budget`] reads it, at least
/// [`MIN_MEM`].
///
/// # Errors
///
/// The parse error, or the value is below [`MIN_MEM`].
pub fn parse_mem(s: &str) -> Result<Bytes, String> {
    let mem = Bytes::parse_budget(s).map_err(|e| e.to_string())?;
    if mem < MIN_MEM {
        return Err(format!(
            "size {s:?} must be at least {}",
            MIN_MEM.to_mem_arg()
        ));
    }
    Ok(mem)
}

/// Diff large text files under a hard memory limit.
#[derive(Debug, Parser)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one bool per command-line flag"
)]
#[command(
    name = "rift",
    version,
    args_conflicts_with_subcommands = true,
    subcommand_negates_reqs = true,
    disable_help_subcommand = true
)]
pub struct Args {
    /// A program run by Rift instead of a diff of two files. A file of the
    /// same name is given with a path: `./serve`.
    #[command(subcommand)]
    pub command: Option<Command>,
    /// First file. It must be a regular file: Rift reads at offsets, so a
    /// pipe, FIFO or `<(command)` is refused. `-` is a file name, not
    /// standard input. Two directories are compared as trees: files only
    /// in one tree, renamed files (same content) and a diff of each
    /// changed file, in the format of `diff -r`.
    #[arg(required = true)]
    pub a: Option<PathBuf>,
    /// Second file, or second directory, under the same rules as the first.
    #[arg(required = true)]
    pub b: Option<PathBuf>,
    /// Print line counts, the segment map and resource use instead of a
    /// diff. For two directories, one status per path (A, D, M, R, T) and
    /// the counts.
    #[arg(long)]
    pub summary: bool,
    /// Heap limit, in bytes or with a K, M, G or T unit (powers of 1024),
    /// at least 64K. The output is identical on any machine for the same
    /// --mem and input. A full diff is refused below 10.3 MiB
    /// (10475K), whatever the files hold, for the buffers that compare
    /// the lines of a region, and a pair with many changed regions needs
    /// more; --summary and --key go lower. A refused run names a --mem to
    /// rerun with, at least twice the one refused and, for a full diff,
    /// at least 10475K. It covers what the refused step needed; a later
    /// step can ask for more.
    #[arg(long, default_value = "256M", value_parser = parse_mem)]
    pub mem: Bytes,
    /// Browse the diff in the terminal instead of printing it.
    #[arg(short, long, conflicts_with = "summary")]
    pub interactive: bool,
    /// Skip the search for moved blocks. The diff is the same.
    #[arg(long, conflicts_with = "key")]
    pub no_moves: bool,
    /// After a diff, print peak heap, time and approximate regions to
    /// standard error, and after an error the peak heap. With --summary it
    /// adds nothing: the summary prints the heap and the time, and refines
    /// nothing.
    #[arg(long)]
    pub stats: bool,
    /// Compare records by key instead of lines, regardless of row order:
    /// csv:N (column, from 1), csv:NAME (header), json:PATH (dotted field)
    /// or copy:NAME (COPY column of a Postgres dump).
    #[arg(long, value_name = "SPEC", conflicts_with_all = ["summary", "interactive"])]
    pub key: Option<rift_core::keyed::KeySpec>,
    /// With --key, print the records under each change: the one from A
    /// behind "  - ", the one from B behind "  + ", each of their lines
    /// behind the same prefix.
    #[arg(long, requires = "key")]
    pub show_records: bool,
    /// With --key, print under each changed key the fields that differ,
    /// one per line: "    NAME: OLD -> NEW". Values print as the file
    /// has them, cut at 64 bytes with "..."; "(none)" stands for a field
    /// the record lacks.
    #[arg(long, requires = "key")]
    pub show_fields: bool,
    /// With --key, compare only these fields, comma-separated: header
    /// names, column numbers for csv:N, top-level names for json:, column
    /// names for copy:. A key whose other fields changed is not reported.
    /// Implies --show-fields.
    #[arg(long, requires = "key", value_name = "NAMES", value_delimiter = ',')]
    pub fields: Option<Vec<String>>,
}

/// What Rift runs instead of a diff of two files.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Serve Rift's protocol, JSON-RPC 2.0, on standard input and output.
    Serve(ServeArgs),
    /// The command git runs for an external diff: `diff.<driver>.command`.
    #[command(name = "git-diff")]
    GitDiff(GitDiffArgs),
}

/// Arguments of `rift serve`.
#[derive(Debug, clap::Args)]
pub struct ServeArgs {
    /// Most memory one session may ask for, as for --mem.
    #[arg(long, default_value = "1G", value_parser = Bytes::parse_budget)]
    pub max_session_mem: Bytes,
    /// Memory for all sessions together and the protocol's buffers.
    #[arg(long, default_value = "2G", value_parser = Bytes::parse_budget)]
    pub max_total_mem: Bytes,
    /// On exit, print the peak heap to standard error.
    #[arg(long)]
    pub stats: bool,
}

/// Arguments of `rift git-diff`: options, then what git passes.
#[derive(Debug, clap::Args)]
pub struct GitDiffArgs {
    /// Heap limit, in bytes or with a K, M, G or T unit (powers of 1024),
    /// at least 64K. The output is identical on any machine for the same
    /// --mem and input.
    #[arg(long, default_value = "256M", value_parser = parse_mem)]
    pub mem: Bytes,
    /// After the diff or an error, print the peak heap to standard error.
    #[arg(long)]
    pub stats: bool,
    /// `path old-file old-hex old-mode new-file new-hex new-mode`, and for a
    /// rename or copy `new-path` and the rename header. A single argument
    /// is an unmerged path: prints `* Unmerged path <path>` and exits 0.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub external: Vec<OsString>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_help_gives_the_full_diff_floor() {
        let help = Args::command().render_long_help().to_string();
        let floor = FULL_DIFF_MEM.to_mem_arg();
        assert!(help.contains(&format!("({floor})")), "{help}");
    }
}
