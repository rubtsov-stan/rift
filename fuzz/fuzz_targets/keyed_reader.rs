//! Key mode's record reader alone, for every key kind, with window and chunk
//! counts from the input. Property: any chunk count reads the same records
//! as one, a rewound reader reads them again, keys hash as `key_hash`
//! says and match an independent parse of simple records, and a filter on
//! half the hash range keeps exactly that half.
//!
//! The check, and the input layout, are in `src/keyed_reader.rs`.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| rift_fuzz::keyed_reader::check(data));
