//! `--summary`: the segment map and what it cost, without a diff.

use std::process::ExitCode;
use std::time::Instant;

use rift_core::skeleton::build_pair;
use rift_core::{Budget, Bytes, SegmentStatus, Source, alloc};

/// Differing ranges listed before the rest are only counted.
const SHOWN_RANGES: usize = 10;

/// Builds the map of `a` and `b` under `mem` and prints it. Exit code 0
/// when the files are identical, 1 when they differ.
pub fn run<A: Source, B: Source>(a: &A, b: &B, mem: Bytes) -> anyhow::Result<ExitCode> {
    let start = Instant::now();
    let budget = Budget::new(mem);
    let (scan_a, scan_b, map) = build_pair(a, b, &budget)?;
    let elapsed = start.elapsed();

    let segments = map.segments();
    println!(
        "a: {} lines, {} bytes",
        scan_a.index().line_count(),
        a.len()
    );
    println!(
        "b: {} lines, {} bytes",
        scan_b.index().line_count(),
        b.len()
    );
    println!("stratum {}, {} anchors", map.level(), map.anchors());
    for status in [
        SegmentStatus::Equal,
        SegmentStatus::Differs,
        SegmentStatus::Approx,
    ] {
        let (count, lines_a, lines_b) = segments
            .iter()
            .filter(|s| s.status == status)
            .fold((0, 0, 0), |(n, a, b), s| {
                (n + 1, a + s.len_a(), b + s.len_b())
            });
        println!(
            "{:<8} {count} segments, {lines_a} lines in a, {lines_b} in b",
            format!("{status:?}").to_lowercase(),
        );
    }
    let mut differing = segments
        .iter()
        .filter(|s| s.status != SegmentStatus::Equal)
        .peekable();
    if differing.peek().is_some() {
        println!("first differing ranges (0-based, end exclusive):");
    }
    for s in differing.by_ref().take(SHOWN_RANGES) {
        println!(
            "  a {}..{}  b {}..{}",
            s.a.start, s.a.end, s.b.start, s.b.end
        );
    }
    let more = differing.count();
    if more > 0 {
        println!("  ... and {more} more");
    }
    println!("peak heap {} of {}", human(alloc::peak()), human(mem));
    println!("time {:.2} s", elapsed.as_secs_f64());
    Ok(if map.is_identical() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// A byte count in the largest binary unit that keeps it at least 1.
#[allow(clippy::cast_precision_loss, reason = "display only")]
fn human(b: Bytes) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    let Some(unit) = (0..UNITS.len()).rev().find(|&u| b.0 >> (10 * (u + 1)) > 0) else {
        return format!("{} B", b.0);
    };
    let value = b.0 as f64 / (1u64 << (10 * (unit + 1))) as f64;
    format!("{value:.1} {}", UNITS[unit])
}
