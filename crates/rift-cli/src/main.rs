//! Command-line entry point for Rift.
#![deny(unsafe_code)]

mod full;
mod gitdiff;
mod keyed;
mod lfs;
mod serve;
mod source;
mod summary;

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use rift_core::alloc::CountingAlloc;
use rift_core::{Budget, BudgetExceeded, Bytes};
use rift_diff::{Args, Command};

use crate::source::FileSource;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Heap kept out of the budget for what it does not count: parsing the
/// command line, standard output's buffers, error and advice messages,
/// thread records, and the heap of rayon's global pool, which the scan
/// and the skeleton start. The pool grows with its thread count, so the
/// reserve has a part per pool thread; see [`pool_threads`].
///
/// Sized from a trace of every allocation on macOS, 8 threads (1 to 32
/// for the pool), at `--mem` 1 to 12M in every mode, refusals and error
/// exits included: at most 30 KiB outside the pool and 7.5 KiB per pool
/// thread, rounded up to 1.5 times that. `tests/small_budget_heap.rs` and
/// the small budgets of `bench/ci-mem.sh` fail when an untracked
/// allocation outgrows it.
const RESERVE: Bytes = Bytes(48 << 10);
/// The reserve's part per thread; see [`RESERVE`].
const RESERVE_PER_THREAD: Bytes = Bytes(12 << 10);

/// The heap `--mem` keeps out of the budget for this run.
fn reserve() -> Bytes {
    Bytes(RESERVE.0 + RESERVE_PER_THREAD.0 * pool_threads() as u64)
}

/// The threads rayon's global pool will start, by rayon's own rule
/// (`ThreadPoolBuilder::get_num_threads` in rayon-core): a positive
/// `RAYON_NUM_THREADS`, else, unless that is 0, a positive
/// `RAYON_RS_NUM_CPUS`, else the cores. Asking rayon would start the
/// pool, whose heap a small budget cannot hold.
fn pool_threads() -> usize {
    let var = |name| std::env::var(name).ok()?.parse::<usize>().ok();
    match var("RAYON_NUM_THREADS") {
        Some(n @ 1..) => n,
        Some(0) => full::threads(),
        None => var("RAYON_RS_NUM_CPUS")
            .filter(|&n| n > 0)
            .unwrap_or_else(full::threads),
    }
}

/// The budget of a run under `--mem mem`: what is left after
/// [`reserve`], split into the two halves.
fn budget(mem: Bytes) -> Budget {
    Budget::new(Bytes(mem.0.saturating_sub(reserve().0)))
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
    let (Some(path_a), Some(path_b)) = (&args.a, &args.b) else {
        unreachable!("clap requires both files when no subcommand is given");
    };
    let a = FileSource::open(path_a).with_context(|| format!("open {}", path_a.display()))?;
    let b = FileSource::open(path_b).with_context(|| format!("open {}", path_b.display()))?;
    if let Some(spec) = &args.key {
        return keyed::run(
            &a,
            &b,
            [path_a, path_b],
            spec,
            args.mem,
            args.stats,
            args.show_records,
        );
    }
    if args.summary {
        return summary::run(&a, &b, args.mem, !args.no_moves);
    }
    let files = full::Files {
        a: Arc::new(a),
        b: Arc::new(b),
        path_a,
        path_b,
        moves: !args.no_moves,
    };
    if args.interactive {
        full::interactive(&files, args.mem)
    } else {
        full::unified(&files, args.mem, args.stats)
    }
}

fn main() -> ExitCode {
    let args = Args::parse_from(tui_alias(std::env::args_os().collect()));
    match &args.command {
        Some(Command::Serve(cli)) => return serve::main(cli),
        Some(Command::GitDiff(cli)) => return gitdiff::main(cli),
        None => {}
    }
    match run(&args) {
        Ok(code) => code,
        // Exit as a shell reports a `diff` killed by SIGPIPE: 128 + 13.
        Err(err) if broken_pipe(&err) => ExitCode::from(141),
        Err(err) => {
            match mem_advice(&err, args.mem) {
                Some(advice) => eprintln!("rift: {err:#}; {advice}"),
                None => eprintln!("rift: {err:#}"),
            }
            if args.stats {
                eprintln!("{}", summary::peak_heap(args.mem));
            }
            ExitCode::from(2)
        }
    }
}

/// The output's reader went away, as in `rift a b | head`. Rust ignores
/// SIGPIPE, so this arrives as a failed write, and every write to standard
/// output reports it as [`rift_core::Error::Write`].
fn broken_pipe(err: &anyhow::Error) -> bool {
    err.chain()
        .any(|e| matches!(e.downcast_ref(), Some(rift_core::Error::Write(w)) if w.kind() == std::io::ErrorKind::BrokenPipe))
}

/// For a budget refusal, a `--mem` to rerun with: what
/// [`BudgetExceeded::mem_advice`] asks of the budget, plus the reserve.
fn mem_advice(err: &anyhow::Error, mem: Bytes) -> Option<String> {
    let refusal = err.chain().find_map(|e| match e.downcast_ref() {
        Some(rift_core::Error::Budget(b)) => Some(*b),
        _ => e.downcast_ref::<BudgetExceeded>().copied(),
    })?;
    let reserve = reserve();
    let need = refusal.mem_advice(Bytes(mem.0.saturating_sub(reserve.0)))?;
    Some(format!(
        "rerun with --mem {}",
        Bytes(need.0.checked_add(reserve.0)?).to_mem_arg()
    ))
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
    fn the_reserve_counts_the_threads_rayon_starts() {
        assert_eq!(pool_threads(), rayon::current_num_threads());
    }

    #[test]
    fn tui_is_interactive_unless_it_is_a_file() {
        assert_eq!(alias(&["rift", "tui", "a", "b"]), ["rift", "-i", "a", "b"]);
        assert_eq!(alias(&["rift", "tui", "other"]), ["rift", "tui", "other"]);
        assert_eq!(alias(&["rift", "a", "tui", "b"]), ["rift", "a", "tui", "b"]);
        let args = Args::parse_from(alias(&["rift", "tui", "--mem", "1G", "a", "b"]));
        assert!(args.interactive);
    }

    #[test]
    fn a_budget_refusal_names_a_mem_to_rerun_with() {
        let refusal = |half, requested, available| {
            anyhow::Error::from(rift_core::Error::Budget(BudgetExceeded {
                half,
                requested: Bytes(requested),
                available: Bytes(available),
            }))
        };
        let mem = Bytes(Bytes::mib(7).0 + reserve().0);
        // 3.5 MiB per half, 0.5 MiB of it free: 3 MiB held, 5 MiB needed,
        // and the reserve on top.
        let err = refusal(rift_core::Half::Refine, 2 << 20, 512 << 10);
        let advised = format!(
            "rerun with --mem {}",
            Bytes(Bytes::mib(10).0 + reserve().0).to_mem_arg()
        );
        assert_eq!(mem_advice(&err, mem), Some(advised));
        let err = err.context("the changes printed above are incomplete");
        assert!(mem_advice(&err, mem).is_some(), "found under a context");
        let fits = refusal(rift_core::Half::Skeleton, 10, 20);
        assert_eq!(mem_advice(&fits, mem), None);
        assert_eq!(mem_advice(&anyhow::anyhow!("other"), mem), None);
    }
}
