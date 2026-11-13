//! The diff itself: a unified diff on standard output, or the terminal
//! interface.

use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Instant, UNIX_EPOCH};

use anyhow::Context;
use rift_core::diff::{Config, Diff};
use rift_core::unified::{DEFAULT_CONTEXT, Label, Options, Timestamp, Verdict, quote_diff_name};
use rift_core::{ApproxReason, Bytes};

use crate::source::FileSource;
use crate::summary::{peak_heap, print_moves};

/// The two inputs, opened.
pub struct Files<'p> {
    pub a: Arc<FileSource>,
    pub b: Arc<FileSource>,
    pub path_a: &'p Path,
    pub path_b: &'p Path,
    /// Look for moved blocks.
    pub moves: bool,
}

/// Refinement workers to start under `--mem mem`: what it allows, at most
/// one per core. The diff reserves for what `--mem` allows either way.
fn threads(mem: Bytes) -> usize {
    rift_core::budget::threads(mem).min(rift_core::budget::cores())
}

pub fn build(files: &Files<'_>, mem: Bytes) -> anyhow::Result<Diff<FileSource, FileSource>> {
    let budget = crate::budget(mem, true);
    Ok(Diff::with_config(
        Arc::clone(&files.a),
        Arc::clone(&files.b),
        &budget,
        threads(mem),
        Config {
            moves: files.moves,
            ..Config::default()
        },
    )?)
}

/// Writes the unified diff to standard output. Exit code 0 when the files
/// are identical, 1 when they differ.
pub fn unified(files: &Files<'_>, mem: Bytes, stats: bool) -> anyhow::Result<ExitCode> {
    let start = Instant::now();
    let diff = build(files, mem)?;
    let (old_name, old_time) = label(files.path_a)?;
    let (new_name, new_time) = label(files.path_b)?;
    let opts = Options {
        context: DEFAULT_CONTEXT,
        old: Label {
            name: &old_name,
            mtime: Some(old_time),
        },
        new: Label {
            name: &new_name,
            mtime: Some(new_time),
        },
    };
    let written = diff.write_unified(&opts, io::stdout().lock())?;
    if stats {
        eprintln!("{}", peak_heap(mem));
        eprintln!("time {:.2} s", start.elapsed().as_secs_f64());
        eprintln!(
            "approx {} segments, {} lines in a and b",
            written.approx_segments, written.approx_lines
        );
        for (reason, n) in ApproxReason::ALL.iter().zip(written.approx_reasons) {
            if n > 0 {
                eprintln!(
                    "  {n} segments: {}{}",
                    reason.describe(),
                    rerun(*reason, mem)
                );
            }
        }
        print_moves(
            &mut io::stderr().lock(),
            diff.moves(),
            diff.off_chain().dropped(),
        )?;
    }
    Ok(match written.verdict {
        Verdict::Identical => ExitCode::SUCCESS,
        Verdict::Different => ExitCode::from(1),
    })
}

/// For a segment that ran out of budget, a `--mem` to try: twice this one,
/// the step a refusal advises. Other reasons do not depend on `--mem`.
fn rerun(reason: ApproxReason, mem: Bytes) -> String {
    match (reason, mem.0.checked_mul(2)) {
        (ApproxReason::Budget, Some(twice)) => {
            format!("; try --mem {}", Bytes(twice).to_mem_arg())
        }
        _ => String::new(),
    }
}

/// Without the `tui` feature there is no terminal interface to open.
#[cfg(not(feature = "tui"))]
pub fn interactive(_: &Files<'_>, _: Bytes) -> anyhow::Result<ExitCode> {
    anyhow::bail!("built without TUI support")
}

/// Opens the terminal interface on the diff. Exit code as for the diff.
#[cfg(feature = "tui")]
pub fn interactive(files: &Files<'_>, mem: Bytes) -> anyhow::Result<ExitCode> {
    use std::io::IsTerminal;
    // Before the scan, which can take minutes on a large pair.
    anyhow::ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "-i needs a terminal"
    );
    let diff = build(files, mem)?;
    let identical = rift_core::DiffView::segments(&diff)
        .iter()
        .all(|s| s.status == rift_core::SegmentStatus::Equal);
    let mut app = rift_tui::App::new(&diff, diff.lines_a(), diff.lines_b());
    let mut term = ratatui::init();
    let res = rift_tui::run(&mut term, &mut app, rift_tui::crossterm_events, None);
    ratatui::restore();
    res.context("terminal")?;
    Ok(if identical {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// The header of a file as `diff -u` prints it: the name, quoted as GNU diff
/// quotes it, and the modification time in the local zone.
fn label(path: &Path) -> anyhow::Result<(Vec<u8>, Timestamp)> {
    let modified = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .with_context(|| format!("stat {}", crate::source::shown(path)))?;
    let (secs, nanos) = match modified.duration_since(UNIX_EPOCH) {
        Ok(d) => (
            i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
            d.subsec_nanos(),
        ),
        Err(e) => {
            let d = e.duration();
            // Before the epoch: whole seconds down, nanoseconds up.
            let back = i64::try_from(d.as_secs()).unwrap_or(i64::MAX);
            match d.subsec_nanos() {
                0 => (-back, 0),
                n => (-back - 1, 1_000_000_000 - n),
            }
        }
    };
    Ok((
        quote_diff_name(path.as_os_str().as_bytes()).into_owned(),
        Timestamp {
            secs,
            nanos,
            utc_offset: clock::utc_offset(secs),
        },
    ))
}

/// The local zone's offset from UTC. The only other FFI in the binary.
#[allow(unsafe_code, reason = "localtime_r has no safe std wrapper")]
mod clock {
    /// Offset east of UTC, in seconds, at Unix time `secs`; 0 if unknown.
    pub(super) fn utc_offset(secs: i64) -> i32 {
        // time_t is 64 bits on every target the binary is built for.
        let t: libc::time_t = secs;
        // SAFETY: `tm` is plain data for which all-zero bytes are valid,
        // and localtime_r writes only into the struct we pass, reading `t`.
        unsafe {
            let mut tm: libc::tm = std::mem::zeroed();
            if libc::localtime_r(&raw const t, &raw mut tm).is_null() {
                return 0;
            }
            i32::try_from(tm.tm_gmtoff).unwrap_or(0)
        }
    }
}
