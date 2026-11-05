//! `--key`: records compared by a key column, independent of row order.

use std::process::ExitCode;

use rift_core::Bytes;
use rift_core::keyed::KeySpec;

use crate::source::FileSource;

/// Compares `a` and `b` record by record, keyed by `spec`, under `mem`.
///
/// # Errors
///
/// Always, for now: key mode is not written yet.
pub fn run(
    _a: &FileSource,
    _b: &FileSource,
    _spec: &KeySpec,
    _mem: Bytes,
    _stats: bool,
) -> anyhow::Result<ExitCode> {
    anyhow::bail!("key mode is not implemented yet")
}
