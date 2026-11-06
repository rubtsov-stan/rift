//! `--summary`: the segment map and what it cost, without a diff.

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Instant;

use rift_core::index::IndexedLines;
use rift_core::moves::Moves;
use rift_core::skeleton::build_pair;
use rift_core::{Budget, Bytes, Half, SegmentStatus, Source, alloc};

/// Differing ranges, and moved blocks, listed before the rest are only
/// counted.
const SHOWN_RANGES: usize = 10;
/// Read buffer of the two readers that compare moved lines.
const MOVES_SCRATCH: Bytes = Bytes(4 << 10);

/// Builds the map of `a` and `b` under `mem` and prints it. Exit code 0
/// when the files are identical, 1 when they differ.
pub fn run<A: Source, B: Source>(a: &A, b: &B, mem: Bytes) -> anyhow::Result<ExitCode> {
    let start = Instant::now();
    let budget = Budget::new(mem);
    let (scan_a, scan_b, map) = build_pair(a, b, &budget)?;
    let lines_a = IndexedLines::new(a, scan_a.index(), &budget, Half::Refine, MOVES_SCRATCH)?;
    let lines_b = IndexedLines::new(b, scan_b.index(), &budget, Half::Refine, MOVES_SCRATCH)?;
    let off_chain = map.off_chain();
    let moves = Moves::find(
        off_chain.pairs(),
        map.segments(),
        &lines_a,
        &lines_b,
        MOVES_SCRATCH,
        &budget,
    )?;
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
    print_moves(&mut io::stdout().lock(), &moves, off_chain.dropped())?;
    println!("{}", peak_heap(mem));
    println!("time {:.2} s", elapsed.as_secs_f64());
    Ok(if map.is_identical() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

/// The peak heap line: rounded for people, then exact in bytes, so a
/// check against `--mem` does not depend on rounding.
pub fn peak_heap(mem: Bytes) -> String {
    let peak = alloc::peak();
    format!(
        "peak heap {} of {} ({} bytes)",
        human(peak),
        human(mem),
        peak.0
    )
}

/// A byte count in the largest binary unit that keeps it at least 1.
#[allow(clippy::cast_precision_loss, reason = "display only")]
pub fn human(b: Bytes) -> String {
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    let Some(unit) = (0..UNITS.len()).rev().find(|&u| b.0 >> (10 * (u + 1)) > 0) else {
        return format!("{} B", b.0);
    };
    let value = b.0 as f64 / (1u64 << (10 * (unit + 1))) as f64;
    format!("{value:.1} {}", UNITS[unit])
}

/// Prints the moved blocks, the first few by range, as `--summary` and
/// `--stats` show them; candidates the skeleton had no room for are
/// counted, since some moves may be missing because of them.
pub fn print_moves(out: &mut impl Write, moves: &Moves, dropped: u64) -> io::Result<()> {
    if moves.skipped() {
        return writeln!(out, "moved    not searched, the budget is too small");
    }
    let blocks = moves.blocks();
    let lines: u64 = blocks.iter().map(|m| m.a.end - m.a.start).sum();
    write!(out, "moved    {} blocks, {lines} lines", blocks.len())?;
    if dropped > 0 {
        write!(
            out,
            "; {dropped} candidates dropped, some moves may be missing"
        )?;
    }
    writeln!(out)?;
    for m in blocks.iter().take(SHOWN_RANGES) {
        writeln!(
            out,
            "  a {}..{}  b {}..{}",
            m.a.start, m.a.end, m.b.start, m.b.end
        )?;
    }
    if blocks.len() > SHOWN_RANGES {
        writeln!(out, "  ... and {} more", blocks.len() - SHOWN_RANGES)?;
    }
    Ok(())
}
