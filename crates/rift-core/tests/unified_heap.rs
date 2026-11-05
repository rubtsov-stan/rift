//! The unified writer's own heap does not grow with the size of a hunk.
//!
//! Alone in its binary because it reads the process-wide allocator
//! counters.

use rift_core::alloc::{self, CountingAlloc};
use rift_core::unified::{Label, Options, write_diff};
use rift_core::{Edit, Hunk, LineNo, MemLines};

#[global_allocator]
static ALLOC: CountingAlloc = CountingAlloc;

/// Every other line changes, so the whole file is one unified hunk of
/// 3 * PAIRS edits.
const PAIRS: u64 = 200_000;

#[test]
fn one_huge_hunk_costs_the_writer_a_bounded_amount() {
    let (mut a, mut b) = (Vec::new(), Vec::new());
    for i in 0..PAIRS {
        a.extend_from_slice(format!("same {i}\nold {i}\n").as_bytes());
        b.extend_from_slice(format!("same {i}\nnew {i}\n").as_bytes());
    }
    let hunks: Vec<Hunk> = (0..PAIRS)
        .map(|i| Hunk {
            edits: vec![
                Edit::Equal {
                    a: LineNo(2 * i),
                    b: LineNo(2 * i),
                    len: 1,
                },
                Edit::Delete {
                    a: LineNo(2 * i + 1),
                    len: 1,
                },
                Edit::Insert {
                    b: LineNo(2 * i + 1),
                    len: 1,
                },
            ],
            approx: false,
        })
        .collect();
    let (la, lb) = (MemLines::new(&a), MemLines::new(&b));
    let label = |name: &'static str| Label {
        name: name.as_bytes(),
        mtime: None,
    };
    let opts = Options {
        context: 3,
        old: label("a"),
        new: label("b"),
    };
    let base = alloc::current().0;
    alloc::reset_peak();
    write_diff(&la, &lb, |i| Ok(hunks.get(i)), &opts, std::io::sink()).unwrap();
    let used = alloc::peak().0 - base;
    // The 1 MiB output buffer and a line buffer; the edits of the open hunk
    // would be 9.6 MB and more.
    assert!(used < 2 << 20, "writer peak {used} bytes");
}
