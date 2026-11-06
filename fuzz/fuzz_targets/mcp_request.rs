//! rift-mcp: a client JSON-RPC message parsed from the input, and a tool
//! call with the input as arguments over an in-process pipe. Property: no
//! panic or hang, and answers keep the documented size bounds.
//!
//! The check, and the input layout, are in `src/mcp.rs`.
#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| rift_fuzz::mcp::check(data));
