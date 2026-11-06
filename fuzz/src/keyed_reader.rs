//! Key mode's record reader alone: `keyed::read::Reader` and its CSV,
//! NDJSON and `COPY` parsers, through `keyed::fuzz_access`.

use std::sync::Mutex;

use rift_core::keyed::fuzz_access::{Filter, Reader, Tables, key_hash};
use rift_core::keyed::{KeySpec, Side};
use rift_core::{Budget, Bytes, Result};
use serde_json::Value;

const SPECS: [&str; 8] = [
    "csv:1",
    "csv:2",
    "csv:id",
    "json:id",
    "json:actor.id",
    "copy:id",
    "copy:rental_id",
    "csv:3",
];
const WINDOWS: [usize; 8] = [64, 65, 100, 256, 1000, 4096, 1 << 16, 1 << 20];
/// Half the key hash range, for the filter check.
const MID: u64 = 1 << 62;

/// One record as the reader kept it: first line, key bytes, table, hash.
type Rec = (u64, Vec<u8>, u32, u64);

/// Reads `data` twice with one reader of `chunks` chunks, keeping hashes
/// in `lo..hi`.
fn read(data: &[u8], spec: &KeySpec, window: usize, chunks: usize, hi: u64) -> Result<Vec<Rec>> {
    let budget = Budget::new(Bytes::mib(64));
    let tables = Mutex::new(Tables::new(&budget)?);
    let filter = Filter::new(0, hi, &[]);
    let mut r = Reader::new(data, Side::A, spec, &budget, window, chunks)?;
    let mut runs: Vec<Vec<Rec>> = Vec::new();
    for _ in 0..2 {
        r.rewind();
        let mut got = Vec::new();
        while r.advance(&tables, &filter)? {
            for rec in r.kept() {
                assert!(rec.key.end <= rec.bytes.len(), "key past its record");
                assert!(!rec.bytes.ends_with(b"\n"), "record keeps its terminator");
                let key = &rec.bytes[rec.key.clone()];
                if rec.table == 0 {
                    assert_eq!(rec.hash, key_hash(key, 0), "hash of {key:?}");
                }
                if let Some(text) = std::str::from_utf8(rec.bytes)
                    .ok()
                    .filter(|_| hi == u64::MAX)
                {
                    oracle(spec, text, key);
                }
                got.push((rec.line, key.to_vec(), rec.table, rec.hash));
            }
        }
        assert_eq!(r.consumed(), data.len() as u64, "reader stopped early");
        if hi == u64::MAX {
            assert_eq!(r.records(), got.len() as u64, "record count");
        }
        runs.push(got);
    }
    assert_eq!(runs[0], runs[1], "a rewound reader reads differently");
    Ok(runs.swap_remove(0))
}

/// Checks a key against an independent reading of simple records: a CSV
/// record without quotes split at commas, a JSON object whose path names
/// occur once and that holds no escape, parsed by `serde_json`.
fn oracle(spec: &KeySpec, text: &str, key: &[u8]) {
    match spec {
        KeySpec::Csv(rift_core::keyed::CsvColumn::Index(col)) if !text.contains('"') => {
            let field = text.split(',').nth(*col as usize);
            assert_eq!(
                field.map(str::as_bytes),
                Some(key),
                "column {col} of {text:?}"
            );
        }
        // Escapes are left out: rift matches names by their raw bytes.
        KeySpec::Ndjson(path)
            if !text.contains('\\')
                && path
                    .iter()
                    .all(|p| text.matches(&format!("\"{p}\"")).count() == 1) =>
        {
            let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(text) else {
                return;
            };
            let mut v = Some(Value::Object(obj));
            for p in path {
                v = v.and_then(|v| v.get(p).cloned());
            }
            if let Some(want) = v {
                let got: Value = serde_json::from_slice(key).expect("key is a JSON value");
                assert_eq!(got, want, "{path:?} of {text:?}");
            }
        }
        _ => {}
    }
}

/// Splits records with a window and spec from the first two bytes.
///
/// Properties: 1, 4 and 16 chunks per window read the same records, or
/// all fail; a rewound reader reads the same records again; the reader
/// consumes the whole input; keys lie inside their records and hash as
/// `key_hash` says; simple CSV and NDJSON keys match an independent
/// parse; a filter on half the hash range keeps exactly those records.
///
/// # Panics
///
/// On any property violation.
pub fn check(data: &[u8]) {
    let (c0, c1, text) = match data {
        [c0, c1, text @ ..] => (*c0, *c1, text),
        _ => return,
    };
    let spec: KeySpec = SPECS[usize::from(c0 & 7)].parse().expect("spec");
    let window = WINDOWS[usize::from(c1 & 7)];
    let one = read(text, &spec, window, 1, u64::MAX);
    for chunks in [4, 16] {
        let many = read(text, &spec, window, chunks, u64::MAX);
        match (&one, &many) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "{chunks} chunks read differently"),
            (Err(_), Err(_)) => {}
            (a, b) => panic!(
                "1 chunk: {:?}, {chunks} chunks: {:?}",
                a.as_ref().err(),
                b.as_ref().err()
            ),
        }
    }
    if let Ok(all) = one {
        let low: Vec<Rec> = all.into_iter().filter(|r| r.3 < MID).collect();
        let kept = read(text, &spec, window, 4, MID).expect("the filtered read fails");
        assert_eq!(kept, low, "the filter keeps other records");
    }
}
