//! The terminal interface's line sanitizer alone. Property: no hidden
//! character in the output, one column per character within the budget,
//! a smaller budget gives a prefix, and plain text passes unchanged.
//!
//! The check, and the input layout, are in `src/sanitize.rs`.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| rift_fuzz::sanitize::check(data));
