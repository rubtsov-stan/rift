//! The full diff on two small inputs under a `--mem` from the first byte.
//! Property: the unified diff applied to A gives B, the verdict matches
//! A == B, and the peak heap, read from the counting allocator, stays
//! within `--mem`.
//!
//! The check, and the input layout, are in `src/pipeline.rs`.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| rift_fuzz::pipeline::check(data));
