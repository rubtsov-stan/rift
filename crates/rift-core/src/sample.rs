//! A sampled line: what the scanner keeps per line that falls in a stratum.

use crate::types::{ByteOffset, LineHash, LineNo};

/// One sampled line. 24 bytes, so a 128 MiB half holds 5.6 million.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(C)]
pub struct Sample {
    /// Hash of the full line, terminator included.
    pub hash: LineHash,
    /// Line number in its file.
    pub line: LineNo,
    /// Byte offset of the line start in its file.
    pub offset: ByteOffset,
}

const _: () = assert!(size_of::<Sample>() == 24);
