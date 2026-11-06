//! Key mode on CSV (RFC 4180) records. Property: a small and a large
//! budget report the same changes, or one fails only for a budget
//! reason; a file compared with itself reports nothing; the small run's
//! peak heap stays within its budget.
//!
//! The check, and the input layout, are in `src/keyed.rs`.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| rift_fuzz::keyed::csv(data));
