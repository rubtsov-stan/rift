//! Rendering a cached hunk does not copy its edit script.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters.

use std::io::Write;
use std::sync::Arc;

use rift_core::alloc::{self, CountingAlloc};
use rift_core::diff::Diff;
use rift_core::{Budget, Bytes, DiffView, Edit, HunkState, SegmentStatus};
use rift_mcp::file::FileSource;
use rift_mcp::render::hunk_text;

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// 30 000 equal lines, B with a different one after every tenth: no line
/// is unique, so nothing anchors, and the whole file is one segment with
/// thousands of edits.
fn lines(changed: bool) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..30_000 {
        out.extend_from_slice(b"p\n");
        if changed && i % 10 == 9 {
            out.extend_from_slice(b"q\n");
        }
    }
    out
}

#[test]
fn a_cached_hunk_is_shared_not_copied_when_rendered() {
    let dir = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for (name, seed) in [("a", false), ("b", true)] {
        let path = dir.path().join(name);
        std::fs::File::create(&path)
            .unwrap()
            .write_all(&lines(seed))
            .unwrap();
        paths.push(path.to_str().unwrap().to_owned());
    }
    let a = Arc::new(FileSource::open(&paths[0]).unwrap().0);
    let b = Arc::new(FileSource::open(&paths[1]).unwrap().0);
    let diff = Diff::new(a, b, &Budget::new(Bytes::mib(64)), 2).unwrap();
    let i = diff
        .segments()
        .iter()
        .position(|s| s.status != SegmentStatus::Equal)
        .unwrap();
    // The first call computes the hunk and leaves it in the cache.
    hunk_text(&diff, i, 3, 100).unwrap();
    let (HunkState::Ready(h) | HunkState::Approx(h)) = diff.hunk(i) else {
        panic!("not cached");
    };
    let script = (h.edits.len() * size_of::<Edit>()) as u64;
    drop(h);
    assert!(
        script > 100_000,
        "{script} bytes of edits is too few to tell"
    );

    let before = alloc::current().0;
    alloc::reset_peak();
    hunk_text(&diff, i, 3, 100).unwrap();
    let rise = alloc::peak().0 - before;
    assert!(
        rise < script / 2,
        "rendering a hunk of {script} bytes of edits raised the heap by {rise}"
    );
}
