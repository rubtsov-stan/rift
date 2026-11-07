//! Command-line entry point for Rift.
#![deny(unsafe_code)]

mod full;
mod keyed;
mod source;
mod summary;

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use rift_core::alloc::CountingAlloc;
use rift_core::{Budget, BudgetExceeded, Bytes};
use rift_diff::Args;

use crate::source::FileSource;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

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
    if let Some(spec) = &args.key {
        return keyed::run(&a, &b, [&args.a, &args.b], spec, args.mem, args.stats);
    }
    if args.summary {
        return summary::run(&a, &b, args.mem, !args.no_moves);
    }
    let files = full::Files {
        a: Arc::new(a),
        b: Arc::new(b),
        path_a: &args.a,
        path_b: &args.b,
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
    match run(&args) {
        Ok(code) => code,
        Err(err) => {
            match mem_advice(&err, args.mem) {
                Some(advice) => eprintln!("rift: {err:#}; {advice}"),
                None => eprintln!("rift: {err:#}"),
            }
            ExitCode::from(2)
        }
    }
}

/// For a budget refusal, a `--mem` to rerun with: the half held its
/// capacity minus what was free, and needs that plus the request. Not a
/// lower bound, since what a half holds depends on --mem and a smaller
/// limit can take another path, nor always enough, since a later stage
/// may need more.
fn mem_advice(err: &anyhow::Error, mem: Bytes) -> Option<String> {
    let refusal = err.chain().find_map(|e| match e.downcast_ref() {
        Some(rift_core::Error::Budget(b)) => Some(*b),
        _ => e.downcast_ref::<BudgetExceeded>().copied(),
    })?;
    let cap = Budget::new(mem).capacity(refusal.half).0;
    let held = cap.saturating_sub(refusal.available.0);
    let need = held.checked_add(refusal.requested.0)?.checked_mul(2)?;
    // A refusal that does not exceed its half proves nothing about --mem.
    (need > mem.0).then(|| format!("rerun with --mem {}", size(need)))
}

/// `bytes` rounded up to the largest unit that keeps the rounding under
/// 1/64 of the value, so it stays usable as a --mem argument.
fn size(bytes: u64) -> String {
    let (unit, suffix) = [(40, 'T'), (30, 'G'), (20, 'M')]
        .into_iter()
        .map(|(shift, suffix)| (1u64 << shift, suffix))
        .find(|&(unit, _)| bytes >= 64 * unit)
        .unwrap_or((1 << 10, 'K'));
    format!("{}{suffix}", bytes.div_ceil(unit))
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

    #[test]
    fn a_budget_refusal_names_a_mem_to_rerun_with() {
        let refusal = |half, requested, available| {
            anyhow::Error::from(rift_core::Error::Budget(BudgetExceeded {
                half,
                requested: Bytes(requested),
                available: Bytes(available),
            }))
        };
        let mem = Bytes::mib(7);
        // 3.5 MiB per half, 0.5 MiB of it free: 3 MiB held, 5 MiB needed.
        let err = refusal(rift_core::Half::Refine, 2 << 20, 512 << 10);
        assert_eq!(
            mem_advice(&err, mem).as_deref(),
            Some("rerun with --mem 10240K")
        );
        let err = err.context("the changes printed above are incomplete");
        assert!(mem_advice(&err, mem).is_some(), "found under a context");
        let fits = refusal(rift_core::Half::Skeleton, 10, 20);
        assert_eq!(mem_advice(&fits, mem), None);
        assert_eq!(mem_advice(&anyhow::anyhow!("other"), mem), None);
    }

    #[test]
    fn sizes_round_up_by_less_than_a_64th() {
        assert_eq!(size(1), "1K");
        assert_eq!(size(4145), "5K");
        assert_eq!(size(64 << 20), "64M");
        assert_eq!(size((64 << 20) + 1), "65M");
        assert_eq!(size((64 << 20) - 1), "65536K");
        assert_eq!(size(100 << 30), "100G");
    }
}
