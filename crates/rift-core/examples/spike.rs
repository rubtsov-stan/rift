//! Throwaway spike for hypotheses H1 (anchor coverage) and H2 (scan speed).
//!
//! Usage:
//!   spike scan <file>                     single-threaded and parallel scan throughput
//!   spike mutate <a> <b> <seed> <ratio>   derive b from a with insert/delete/modify
//!   spike anchors <a> <b>                 uniqueness, anchors per stratum, gap sizes
#![allow(
    clippy::all,
    clippy::pedantic,
    missing_docs,
    reason = "measurement code, not shipped"
)]

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::os::unix::fs::FileExt;
use std::time::Instant;

use memchr::{memchr, memchr_iter};
use rayon::prelude::*;
use xxhash_rust::xxh3::xxh3_64;

const BUF: usize = 8 << 20;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("scan") => scan(&args[2]),
        Some("mutate") => mutate(
            &args[2],
            &args[3],
            args[4].parse().unwrap(),
            args[5].parse().unwrap(),
        ),
        Some("anchors") => anchors(&args[2], &args[3]),
        _ => eprintln!("usage: spike scan|mutate|anchors ..."),
    }
}

// ---------------------------------------------------------------- H2: scan

/// Reads `file` sequentially through pread in BUF-sized pieces, hashing every
/// line. Returns (lines, xor of hashes) so the work cannot be optimised away.
fn scan_single(file: &File, len: u64, mut on_line: impl FnMut(u64, u64)) -> u64 {
    let mut buf = vec![0u8; BUF];
    let mut filled = 0usize; // bytes in buf not yet consumed (a partial line)
    let mut pos = 0u64; // file offset of buf[0]
    let mut lines = 0u64;
    let mut line_start = 0u64;
    let mut long: Option<xxhash_rust::xxh3::Xxh3> = None;
    loop {
        let n = file
            .read_at(&mut buf[filled..], pos + filled as u64)
            .unwrap();
        if n == 0 {
            if filled > 0 {
                let h = finish(&mut long, &buf[..filled]);
                on_line(h, line_start);
                lines += 1;
            }
            break;
        }
        let end = filled + n;
        let mut start = 0usize;
        for nl in memchr_iter(b'\n', &buf[..end]) {
            let h = finish(&mut long, &buf[start..nl]);
            on_line(h, line_start);
            lines += 1;
            start = nl + 1;
            line_start = pos + start as u64;
        }
        if start == 0 && end == buf.len() {
            // a line longer than the buffer: hash the prefix streaming
            long.get_or_insert_with(Default::default)
                .update(&buf[..end]);
            pos += end as u64;
            filled = 0;
        } else {
            buf.copy_within(start..end, 0);
            filled = end - start;
            pos += start as u64;
        }
    }
    let _ = len;
    lines
}

fn finish(long: &mut Option<xxhash_rust::xxh3::Xxh3>, tail: &[u8]) -> u64 {
    match long.take() {
        Some(mut h) => {
            h.update(tail);
            h.digest()
        }
        None => xxh3_64(tail),
    }
}

/// Parallel scan over BUF-sized chunks aligned to '\n'.
fn scan_parallel(file: &File, len: u64) -> u64 {
    let chunks = (len + BUF as u64 - 1) / BUF as u64;
    (0..chunks)
        .into_par_iter()
        .map_init(
            || vec![0u8; BUF + (1 << 16)],
            |buf, i| {
                let nominal_start = i * BUF as u64;
                let nominal_end = (nominal_start + BUF as u64).min(len);
                let start = if i == 0 {
                    0
                } else {
                    next_newline(file, nominal_start - 1, len)
                };
                let end = if nominal_end == len {
                    len
                } else {
                    next_newline(file, nominal_end - 1, len)
                };
                let need = (end - start) as usize;
                if buf.len() < need {
                    buf.resize(need, 0);
                }
                file.read_exact_at(&mut buf[..need], start).unwrap();
                let mut lines = 0u64;
                let mut s = 0usize;
                let mut acc = 0u64;
                for nl in memchr_iter(b'\n', &buf[..need]) {
                    acc ^= xxh3_64(&buf[s..nl]);
                    lines += 1;
                    s = nl + 1;
                }
                if s < need {
                    acc ^= xxh3_64(&buf[s..need]);
                    lines += 1;
                }
                std::hint::black_box(acc);
                lines
            },
        )
        .sum()
}

/// Offset just past the first '\n' at or after `from`, or `len`.
fn next_newline(file: &File, from: u64, len: u64) -> u64 {
    let mut probe = [0u8; 1 << 16];
    let mut at = from;
    while at < len {
        let n = file.read_at(&mut probe, at).unwrap();
        if n == 0 {
            break;
        }
        if let Some(i) = memchr(b'\n', &probe[..n]) {
            return at + i as u64 + 1;
        }
        at += n as u64;
    }
    len
}

fn scan(path: &str) {
    let file = File::open(path).unwrap();
    let len = file.metadata().unwrap().len();
    let gb = len as f64 / 1e9;
    let t = Instant::now();
    let lines = scan_single(&file, len, |h, _| {
        std::hint::black_box(h);
    });
    let s = t.elapsed().as_secs_f64();
    println!("single   {lines} lines  {:.2} s  {:.2} GB/s", s, gb / s);
    for threads in [2, 4, 8] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        let t = Instant::now();
        let lines = pool.install(|| scan_parallel(&file, len));
        let s = t.elapsed().as_secs_f64();
        println!(
            "par{threads}     {lines} lines  {:.2} s  {:.2} GB/s",
            s,
            gb / s
        );
    }
}

// ------------------------------------------------------------- mutate real data

fn mutate(a: &str, b: &str, seed: u64, ratio: f64) {
    let mut rng = seed;
    let mut next = move || {
        rng = rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    let mut chance = move |p: f64| ((next() >> 11) as f64 / (1u64 << 53) as f64) < p;
    let reader = BufReader::with_capacity(BUF, File::open(a).unwrap());
    let mut out = BufWriter::with_capacity(BUF, File::create(b).unwrap());
    let mut line = Vec::new();
    let mut reader = reader;
    let (mut ins, mut del, mut modi) = (0u64, 0u64, 0u64);
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line).unwrap() == 0 {
            break;
        }
        if !chance(ratio) {
            out.write_all(&line).unwrap();
            continue;
        }
        match (ins + del + modi) % 3 {
            0 => {
                ins += 1;
                let mut extra = line.clone();
                extra.splice(0..0, b"x".iter().copied());
                out.write_all(&extra).unwrap();
                out.write_all(&line).unwrap();
            }
            1 => del += 1,
            _ => {
                modi += 1;
                let mut m = line.clone();
                if m.len() > 1 {
                    m[0] = m[0].wrapping_add(1);
                }
                out.write_all(&m).unwrap();
            }
        }
    }
    out.flush().unwrap();
    println!("insert {ins} delete {del} modify {modi}");
}

// ----------------------------------------------------------- H1: anchors

struct Scanned {
    hashes: Vec<u64>,
    offsets: Vec<u64>,
    len: u64,
}

fn scan_all(path: &str) -> Scanned {
    let file = File::open(path).unwrap();
    let len = file.metadata().unwrap().len();
    let mut hashes = Vec::new();
    let mut offsets = Vec::new();
    scan_single(&file, len, |h, off| {
        hashes.push(h);
        offsets.push(off);
    });
    Scanned {
        hashes,
        offsets,
        len,
    }
}

fn unique_sorted(hashes: &[u64]) -> Vec<u64> {
    let mut s = hashes.to_vec();
    s.sort_unstable();
    let mut out = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let mut j = i + 1;
        while j < s.len() && s[j] == s[i] {
            j += 1;
        }
        if j - i == 1 {
            out.push(s[i]);
        }
        i = j;
    }
    out
}

/// Longest increasing subsequence over `b` values (indices into pairs).
fn lis(b: &[u64]) -> Vec<usize> {
    let mut tails: Vec<usize> = Vec::new();
    let mut prev: Vec<usize> = vec![usize::MAX; b.len()];
    for (i, &v) in b.iter().enumerate() {
        let k = tails.partition_point(|&t| b[t] < v);
        if k > 0 {
            prev[i] = tails[k - 1];
        }
        if k == tails.len() {
            tails.push(i);
        } else {
            tails[k] = i;
        }
    }
    let mut out = Vec::with_capacity(tails.len());
    let mut cur = *tails.last().unwrap_or(&usize::MAX);
    while cur != usize::MAX {
        out.push(cur);
        cur = prev[cur];
    }
    out.reverse();
    out
}

fn pct(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) as f64 * p) as usize]
}

fn anchors(pa: &str, pb: &str) {
    let t = Instant::now();
    let a = scan_all(pa);
    let b = scan_all(pb);
    println!("scanned both in {:.1} s", t.elapsed().as_secs_f64());
    let (na, nb) = (a.hashes.len(), b.hashes.len());
    println!("lines a={na} b={nb}  bytes a={} b={}", a.len, b.len);
    let ua = unique_sorted(&a.hashes);
    let ub = unique_sorted(&b.hashes);
    println!(
        "unique in a: {:.4}  unique in b: {:.4}",
        ua.len() as f64 / na as f64,
        ub.len() as f64 / nb as f64
    );
    // hashes unique in both, sorted (merge)
    let mut both = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < ua.len() && j < ub.len() {
        match ua[i].cmp(&ub[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                both.push(ua[i]);
                i += 1;
                j += 1;
            }
        }
    }
    println!(
        "unique in both: {} ({:.4} of a lines)",
        both.len(),
        both.len() as f64 / na as f64
    );
    let mut pos_b: HashMap<u64, u64> = HashMap::with_capacity(both.len());
    for (line, &h) in b.hashes.iter().enumerate() {
        if both.binary_search(&h).is_ok() {
            pos_b.insert(h, line as u64);
        }
    }
    // pairs in a order
    let mut pairs: Vec<(u64, u64, u64)> = Vec::with_capacity(both.len());
    for (line, &h) in a.hashes.iter().enumerate() {
        if let Some(&lb) = pos_b.get(&h) {
            if both.binary_search(&h).is_ok() {
                pairs.push((h, line as u64, lb));
            }
        }
    }
    drop(pos_b);
    println!();
    println!(
        "{:>2} {:>10} {:>9} {:>10} {:>10} {:>11} {:>8} {:>8} {:>9} {:>9}  {:>7} {:>7}",
        "L",
        "samples",
        "sampleMB",
        "anchors",
        "lis",
        "gaps",
        "p50ln",
        "p99ln",
        "maxln",
        "maxMB",
        "fit128",
        "fit16"
    );
    for l in 0..=14u32 {
        let mask = (1u64 << l) - 1;
        let samples = a
            .hashes
            .iter()
            .chain(b.hashes.iter())
            .filter(|&&h| h & mask == 0)
            .count();
        let sub: Vec<(u64, u64, u64)> = pairs.iter().copied().filter(|p| p.0 & mask == 0).collect();
        let bs: Vec<u64> = sub.iter().map(|p| p.2).collect();
        let keep = lis(&bs);
        // gaps: before first, between, after last; measured in a-lines + b-lines and a-bytes
        let mut gap_lines: Vec<u64> = Vec::with_capacity(keep.len() + 1);
        let mut gap_bytes: Vec<u64> = Vec::with_capacity(keep.len() + 1);
        let mut prev = (0u64, 0u64); // next free line in a, b
        let mut push_gap = |a0: u64, a1: u64, b0: u64, b1: u64| {
            let la = a1 - a0;
            let lb = b1 - b0;
            let bytes = if a1 < na as u64 {
                a.offsets[a1 as usize]
            } else {
                a.len
            } - a.offsets.get(a0 as usize).copied().unwrap_or(a.len);
            gap_lines.push(la + lb);
            gap_bytes.push(bytes);
            (la, bytes)
        };
        let mut fit128 = 0u64;
        let mut fit16 = 0u64;
        let count_fit = |la: u64, total: u64, fit128: &mut u64, fit16: &mut u64| {
            if total * 32 <= 128 << 20 {
                *fit128 += la;
            }
            if total * 32 <= 16 << 20 {
                *fit16 += la;
            }
        };
        for &k in &keep {
            let (_, la, lb) = sub[k];
            let (ga, _) = push_gap(prev.0, la, prev.1, lb);
            count_fit(ga, (la - prev.0) + (lb - prev.1), &mut fit128, &mut fit16);
            fit128 += 1;
            fit16 += 1; // the anchor line itself
            prev = (la + 1, lb + 1);
        }
        let (ga, _) = push_gap(prev.0, na as u64, prev.1, nb as u64);
        count_fit(
            ga,
            (na as u64 - prev.0) + (nb as u64 - prev.1),
            &mut fit128,
            &mut fit16,
        );
        let mut gl = gap_lines.clone();
        gl.sort_unstable();
        let max_bytes = gap_bytes.iter().copied().max().unwrap_or(0);
        println!(
            "{l:>2} {samples:>10} {:>9.1} {:>10} {:>10} {:>11} {:>8} {:>8} {:>9} {:>9.1}  {:>7.4} {:>7.4}",
            samples as f64 * 24.0 / (1 << 20) as f64,
            sub.len(),
            keep.len(),
            gl.len(),
            pct(&gl, 0.5),
            pct(&gl, 0.99),
            gl.last().copied().unwrap_or(0),
            max_bytes as f64 / (1 << 20) as f64,
            fit128 as f64 / na as f64,
            fit16 as f64 / na as f64,
        );
    }
}
