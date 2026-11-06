//! rift-mcp's tools and `render::hunk_text` without the protocol, on two
//! scratch files. Property: no panic or hang, answers keep the documented
//! bounds, and a rendered hunk has one text line per shown line.
//!
//! The check, and the input layout, are in `src/mcp_tools.rs`.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| rift_fuzz::mcp_tools::check(data));
