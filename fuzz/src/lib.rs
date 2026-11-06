//! Properties the fuzz targets check, one module per target family.
//!
//! The targets in `fuzz_targets/` are one line each; the checks live here
//! so that `cargo test` can replay the seed corpora and saved crash inputs
//! without libFuzzer (see `tests` below and the README).
//!
//! Inputs share one layout: leading configuration bytes, then fields
//! separated by the first NUL bytes (see [`fields`]). A NUL inside the last
//! field is data.

#[cfg(not(fuzzing))]
compile_error!(
    "the targets reach internals behind `--cfg fuzzing`: cargo fuzz sets it; \
     for tests, RUSTFLAGS=\"--cfg fuzzing\" cargo test"
);

use rift_core::Bytes;
use rift_core::alloc::{self, CountingAlloc};

pub mod keyed;
pub mod keyed_reader;
pub mod mcp;
pub mod mcp_tools;
pub mod pipeline;
pub mod sanitize;
pub mod scan;
pub mod tui;
pub mod unified;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Splits `data` into `N` fields at the first `N - 1` NUL bytes. Missing
/// fields are empty.
#[must_use]
pub fn fields<const N: usize>(mut data: &[u8]) -> [&[u8]; N] {
    let mut out = [&[][..]; N];
    for (i, field) in out.iter_mut().enumerate() {
        if i + 1 == N {
            *field = data;
            break;
        }
        match data.iter().position(|&b| b == 0) {
            Some(p) => {
                *field = &data[..p];
                data = &data[p + 1..];
            }
            None => {
                *field = data;
                data = &[];
            }
        }
    }
    out
}

/// The first byte and the rest; 0 and empty for empty input.
#[must_use]
pub fn config(data: &[u8]) -> (u8, &[u8]) {
    match data.split_first() {
        Some((&c, rest)) => (c, rest),
        None => (0, data),
    }
}

/// Runs `f` and returns its result with the heap it added at its peak.
///
/// The counters are process-wide: this holds only while nothing else
/// allocates, which is the case under libFuzzer (one input at a time) and
/// in the single replay test. Lazily built global state (the rayon pool,
/// thread-locals) should be warmed up before the first call, or it is
/// charged to the first input.
pub fn peak_heap<T>(f: impl FnOnce() -> T) -> (T, Bytes) {
    let base = alloc::current().0;
    alloc::reset_peak();
    let out = f();
    (out, Bytes(alloc::peak().0.saturating_sub(base)))
}

/// Applies the unified diff `patch` to `a` and returns the result, an
/// oracle written for obvious correctness: it checks every context and
/// removed line against `a` and every hunk length against its header.
///
/// # Errors
///
/// A message naming the first line of `patch` that does not apply.
pub fn apply_unified(a: &[u8], patch: &[u8]) -> Result<Vec<u8>, String> {
    let lines_a = split_lines(a);
    let mut out = Vec::with_capacity(a.len() + patch.len());
    if patch.is_empty() {
        out.extend_from_slice(a);
        return Ok(out);
    }
    let mut recs: Vec<&[u8]> = patch.split(|&b| b == b'\n').collect();
    if recs.pop() != Some(&[][..]) {
        return Err("patch does not end with a newline".into());
    }
    let mut i = 0;
    for prefix in [&b"--- "[..], b"+++ "] {
        if !recs.get(i).is_some_and(|r| r.starts_with(prefix)) {
            return Err(format!("record {i}: expected a {prefix:?} header"));
        }
        i += 1;
    }
    let mut next_a = 0usize;
    while i < recs.len() {
        let (start_a, len_a, len_b) =
            parse_header(recs[i]).ok_or_else(|| format!("record {i}: bad hunk header"))?;
        i += 1;
        let first = if len_a == 0 { start_a } else { start_a - 1 };
        if first < next_a || first > lines_a.len() {
            return Err(format!("record {i}: hunk starts at line {first}"));
        }
        for line in &lines_a[next_a..first] {
            out.extend_from_slice(line);
        }
        next_a = first;
        let (mut seen_a, mut seen_b) = (0, 0);
        while seen_a < len_a || seen_b < len_b {
            let rec = *recs.get(i).ok_or("patch ends inside a hunk")?;
            let (&kind, text) = rec.split_first().ok_or(format!("record {i}: empty"))?;
            i += 1;
            let unterminated = recs.get(i) == Some(&&b"\\ No newline at end of file"[..]);
            if unterminated {
                i += 1;
            }
            let mut line = text.to_vec();
            if !unterminated {
                line.push(b'\n');
            }
            if kind == b' ' || kind == b'-' {
                if lines_a.get(next_a) != Some(&&line[..]) {
                    return Err(format!("record {i}: does not match line {next_a} of a"));
                }
                next_a += 1;
                seen_a += 1;
            }
            if kind == b' ' || kind == b'+' {
                out.extend_from_slice(&line);
                seen_b += 1;
            }
            if !matches!(kind, b' ' | b'-' | b'+') {
                return Err(format!("record {i}: unknown line kind {kind:?}"));
            }
        }
        if seen_a != len_a || seen_b != len_b {
            return Err(format!("record {i}: hunk longer than its header"));
        }
    }
    for line in &lines_a[next_a..] {
        out.extend_from_slice(line);
    }
    Ok(out)
}

/// Lines of `data`, terminators included; a last line without one counts.
#[must_use]
pub fn split_lines(data: &[u8]) -> Vec<&[u8]> {
    data.split_inclusive(|&b| b == b'\n').collect()
}

/// `@@ -s[,l] +s[,l] @@` with an optional ` approx` after it.
fn parse_header(rec: &[u8]) -> Option<(usize, usize, usize)> {
    let rec = std::str::from_utf8(rec).ok()?;
    let rest = rec.strip_prefix("@@ -")?;
    let (ranges, tail) = rest.split_once(" @@")?;
    if !tail.is_empty() && tail != " approx" {
        return None;
    }
    let (old, new) = ranges.split_once(" +")?;
    let range = |s: &str| -> Option<(usize, usize)> {
        match s.split_once(',') {
            Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
            None => Some((s.parse().ok()?, 1)),
        }
    };
    let (start_a, len_a) = range(old)?;
    let (_, len_b) = range(new)?;
    Some((start_a, len_a, len_b))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;

    #[test]
    fn fields_split_at_the_first_nuls_only() {
        assert_eq!(fields::<3>(b"a\0b\0c\0d"), [&b"a"[..], b"b", b"c\0d"]);
        assert_eq!(fields::<3>(b"a"), [&b"a"[..], b"", b""]);
    }

    #[test]
    fn the_oracle_applies_what_diff_u_prints() {
        let a = b"one\ntwo\nthree";
        let patch =
            b"--- a\n+++ b\n@@ -2,2 +2,2 @@\n two\n-three\n\\ No newline at end of file\n+3\n";
        assert_eq!(apply_unified(a, patch).unwrap(), b"one\ntwo\n3\n");
        assert!(apply_unified(b"one\nTWO\nthree", patch).is_err());
    }

    type Check = fn(&[u8]);

    /// Replays every seed and every saved crash input through its target's
    /// check. One test, so the allocator counters see one input at a time.
    #[test]
    fn corpora_and_regressions_pass() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let targets: [(&str, Check); 11] = [
            ("scan", scan::check),
            ("keyed_csv", keyed::csv),
            ("keyed_ndjson", keyed::ndjson),
            ("keyed_copy", keyed::copy),
            ("keyed_reader", keyed_reader::check),
            ("pipeline", pipeline::check),
            ("unified", unified::check),
            ("tui_render", tui::check),
            ("sanitize", sanitize::check),
            ("mcp_request", mcp::check),
            ("mcp_tools", mcp_tools::check),
        ];
        for (name, check) in targets {
            for dir in ["corpus", "regressions"] {
                let Ok(entries) = fs::read_dir(root.join(dir).join(name)) else {
                    continue;
                };
                for entry in entries {
                    let path = entry.unwrap().path();
                    eprintln!("{}", path.display());
                    check(&fs::read(&path).unwrap());
                }
            }
        }
    }
}
