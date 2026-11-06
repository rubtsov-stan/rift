//! The full diff: scan, skeleton, refinement, moves and the unified
//! writer, as `rift A B --mem M` runs them.

use std::sync::{Arc, Once};

use rift_core::diff::Diff;
use rift_core::unified::{DEFAULT_CONTEXT, Label, Options, Verdict};
use rift_core::{Budget, Bytes, Error};

use crate::{apply_unified, config, fields, peak_heap};

/// From too small for the front end and one worker (10 MiB is, on the
/// seeds; 12 MiB is enough) to roomy.
const MEM: [u64; 8] = [
    1 << 20,
    8 << 20,
    10 << 20,
    12 << 20,
    16 << 20,
    24 << 20,
    32 << 20,
    64 << 20,
];

static WARM: Once = Once::new();

fn opts() -> Options<'static> {
    let label = Label {
        name: b"x",
        mtime: None,
    };
    Options {
        context: DEFAULT_CONTEXT,
        old: label,
        new: label,
    }
}

/// Diffs A against B under `--mem` from the first byte, with 1 to 4
/// workers.
///
/// Properties: the diff either succeeds or fails with a budget error (the
/// map, the workers or a line do not fit). On success the unified diff
/// applied to A gives B exactly, the verdict is Identical exactly when A
/// equals B, and the peak heap of the whole run, writer included, stays
/// within `--mem`.
///
/// # Panics
///
/// On any property violation.
pub fn check(data: &[u8]) {
    let (cfg, rest) = config(data);
    let [a, b] = fields(rest);
    let mem = Bytes(MEM[usize::from(cfg & 7)]);
    let threads = usize::from(cfg >> 3 & 3) + 1;
    let (a_arc, b_arc): (Arc<[u8]>, Arc<[u8]>) = (a.into(), b.into());
    // A diff line is at most the input line plus a prefix, and a unified
    // hunk covers at least one input line, so this bounds the output; it is
    // reserved up front so the output does not count as the diff's heap.
    let mut out = Vec::with_capacity(64 * (a.len() + b.len()) + 4096);
    let cap = out.capacity();
    // Worker threads and scheduler state built once per process.
    WARM.call_once(|| {
        let d = Diff::new(
            Arc::<[u8]>::from(&b"x\n"[..]),
            Arc::<[u8]>::from(&b"y\n"[..]),
            &Budget::new(Bytes::mib(64)),
            4,
        )
        .expect("warm-up diff");
        d.write_unified(&opts(), std::io::sink())
            .expect("warm-up write");
    });

    let (res, heap) = peak_heap(|| -> rift_core::Result<_> {
        let budget = Budget::new(mem);
        let diff = Diff::new(a_arc, b_arc, &budget, threads)?;
        diff.write_unified(&opts(), &mut out)
    });
    let written = match res {
        Ok(w) => w,
        // A budget too small for the map, the workers or one line.
        Err(Error::Budget(_) | Error::LineTooLong { .. }) => return,
        Err(e) => panic!("diff failed under --mem {mem}: {e}"),
    };
    assert_eq!(out.capacity(), cap, "output outgrew its bound");
    assert!(heap <= mem, "peak heap {heap} over --mem {mem}");
    assert_eq!(
        written.verdict == Verdict::Identical,
        a == b,
        "verdict {:?}",
        written.verdict
    );
    match apply_unified(a, &out) {
        Ok(got) => assert!(got == b, "the diff applied to a does not give b"),
        Err(e) => panic!("the diff does not apply to a: {e}"),
    }
}
