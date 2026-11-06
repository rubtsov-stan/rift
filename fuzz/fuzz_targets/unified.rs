//! The unified writer on scripts built from the input. Property: the
//! output applied to A gives B and the verdict matches the script; a
//! script with one edit moved is accepted exactly when `validate`
//! accepts it.
//!
//! The check, and the input layout, are in `src/unified.rs`.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| rift_fuzz::unified::check(data));
