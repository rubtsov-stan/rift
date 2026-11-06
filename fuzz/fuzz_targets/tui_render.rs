//! The terminal interface's frames, drawn on a test backend after a
//! sequence of keys. Property: no cell holds a control, bidirectional,
//! zero-width or other default-ignorable character, whatever bytes the
//! lines hold.
//!
//! The check, and the input layout, are in `src/tui.rs`.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| rift_fuzz::tui::check(data));
