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
    for s in differing.take(SHOWN_RANGES) {
        println!(
            "  a {}..{}  b {}..{}",
            s.a.start, s.a.end, s.b.start, s.b.end
        );
    }
    println!(
        "peak heap {:.1} MiB of {:.1} MiB",
        mib(alloc::peak()),
        mib(mem)
    );
    println!("time {:.2} s", elapsed.as_secs_f64());
    Ok(if map.is_identical() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    })
}

#[allow(clippy::cast_precision_loss, reason = "display only")]
fn mib(b: Bytes) -> f64 {
    b.0 as f64 / f64::from(1 << 20)
}

/// Parses `256M`, `1G`, `512k` or a plain byte count, above zero.
pub fn parse_size(s: &str) -> Result<Bytes, String> {
    let invalid = || format!("invalid size {s:?}, expected a number with optional K, M, G or T");
    let (digits, shift) = match s.as_bytes().last().map(u8::to_ascii_uppercase) {
        Some(b'K') => (&s[..s.len() - 1], 10),
        Some(b'M') => (&s[..s.len() - 1], 20),
        Some(b'G') => (&s[..s.len() - 1], 30),
        Some(b'T') => (&s[..s.len() - 1], 40),
        _ => (s, 0),
    };
    if !digits.bytes().all(|c| c.is_ascii_digit()) {
        return Err(invalid());
    }
    let n: u64 = digits.parse().map_err(|_| invalid())?;
    if n == 0 {
        return Err(format!("size {s:?} must be above zero"));
    }
    n.checked_mul(1 << shift)
        .map(Bytes)
        .ok_or_else(|| format!("size {s:?} is too large"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_take_binary_suffixes() {
        assert_eq!(parse_size("256M"), Ok(Bytes::mib(256)));
        assert_eq!(parse_size("1g"), Ok(Bytes(1 << 30)));
        assert_eq!(parse_size("512k"), Ok(Bytes(512 << 10)));
        assert_eq!(parse_size("1000"), Ok(Bytes(1000)));
        assert!(parse_size("").is_err());
        assert!(parse_size("M").is_err());
        assert!(parse_size("1.5G").is_err());
        assert!(parse_size("+1M").is_err());
        assert!(parse_size("0").is_err());
        assert!(parse_size("99999999999T").is_err());
    }
}
