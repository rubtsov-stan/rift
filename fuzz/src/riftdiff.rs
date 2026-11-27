//! The `.riftdiff` reader on arbitrary bytes. Under `--cfg fuzzing` the
//! reader skips its checksums, so inputs reach every structural check.

use rift_core::saved::{SavedDiff, SavedFile};
use rift_core::{Budget, Bytes, DiffView, Half, LineAccess, LineBuf, LineNo};

use crate::peak_heap;

/// The budget every read runs under.
const MEM: Bytes = Bytes(16 << 20);
/// Largest input file the check builds in memory to open a view over.
const MAX_FILE: u64 = 1 << 20;

/// Reads the input as a result: header, sections, meta, the whole map,
/// both line indexes, the moves, the chunk tables and every block's
/// scripts; then, for files of up to 1 MiB, opens a view over files of
/// zeros of the saved sizes and asks for every hunk and a few lines.
///
/// Properties: no panic, whatever the input; the peak heap stays within
/// the budget plus 1 MiB of what nothing reserves (the error messages
/// and the meta's small records).
///
/// # Panics
///
/// On any property violation.
pub fn check(data: &[u8]) {
    let ((), peak) = peak_heap(|| {
        let _ = read(data);
    });
    assert!(
        peak.0 <= MEM.0 + (1 << 20),
        "peak heap {} for a {}-byte input",
        peak.0,
        data.len()
    );
}

fn read(data: &[u8]) -> rift_core::Result<()> {
    let budget = Budget::new(MEM);
    let file = SavedFile::open(data, &budget)?;
    let map = file.map(&budget)?;
    file.line_indexes(&budget)?;
    file.moves(&budget)?;
    file.chunk_table(0, &budget, Half::Refine)?;
    file.chunk_table(1, &budget, Half::Refine)?;
    let mut buf = vec![0; usize::try_from(map.max_block).unwrap_or(0)];
    for j in 0..map.blocks.len() {
        file.block_hunks(&map, j, &mut buf)?;
    }
    drop((map, buf));
    let sizes = [file.meta().files[0].size, file.meta().files[1].size];
    if sizes.iter().any(|&s| s > MAX_FILE) {
        return Ok(());
    }
    let (a, b) = (vec![0u8; sizes[0] as usize], vec![0u8; sizes[1] as usize]);
    let view = SavedDiff::open(file, &a[..], &b[..], &budget)?;
    for i in 0..view.segments().len() {
        let _ = view.try_hunk(i);
    }
    let mut lines = LineBuf::new();
    for (n, side) in [
        (view.len_a(), view.lines_a() as &dyn LineAccess),
        (view.len_b(), view.lines_b()),
    ] {
        for line in [0, n / 2, n.saturating_sub(1)] {
            let _ = side.read_lines(LineNo(line)..LineNo((line + 1).min(n)), &mut lines);
        }
    }
    Ok(())
}
