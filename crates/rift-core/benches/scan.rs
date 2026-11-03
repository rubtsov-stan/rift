//! Scan throughput on in-memory data: one thread against all cores.
//!
//! Lines are 82 bytes like the generated SQL dumps, so the numbers compare
//! with the spike table in `DESIGN.md`.

use divan::Bencher;
use divan::counter::BytesCount;
use rift_core::scan::{ScanConfig, scan};
use rift_core::{Budget, Bytes};

const SIZE: usize = 128 << 20;

fn sql_lines(size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size + 128);
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    while out.len() < size {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let line = format!(
            "INSERT INTO rental VALUES ({}, '2024-01-15 10:{:02}:{:02}', {}, {}, '{:016x}');\n",
            out.len() / 82,
            x % 60,
            (x >> 8) % 60,
            (x >> 16) % 100_000,
            (x >> 32) % 1_000,
            x
        );
        out.extend_from_slice(line.as_bytes());
    }
    out
}

fn config(threads: usize) -> ScanConfig {
    ScanConfig {
        level: 7,
        sample_cap: 1 << 20,
        checkpoint_interval: 2048,
        checkpoint_cap: 1 << 16,
        chunk: Bytes::mib(4),
        threads,
    }
}

#[divan::bench(args = [1, 4, 8], sample_count = 10)]
fn scan_threads(bencher: Bencher, threads: usize) {
    let data = sql_lines(SIZE);
    let budget = Budget::new(Bytes::mib(512));
    let cfg = config(threads);
    bencher
        .counter(BytesCount::new(data.len()))
        .bench(|| scan(&data[..], &budget, &cfg).unwrap());
}

fn main() {
    divan::main();
}
