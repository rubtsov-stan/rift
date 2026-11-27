//! The `.riftdiff` reader. Property: no panic and a bounded heap on any
//! input.
//!
//! The check is in `src/riftdiff.rs`.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| rift_fuzz::riftdiff::check(data));
