//! Line scanner. Property: the line count, checkpoints and samples of
//! `scan` match a plain split of the input for any chunk size, thread
//! count and caps, the samples equal those of a one-chunk scan, and an
//! index-backed reader returns every line, whole, as a prefix and in
//! pieces.
//!
//! The check, and the input layout, are in `src/scan.rs`.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| rift_fuzz::scan::check(data));
