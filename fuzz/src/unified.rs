//! The unified writer, `rift_core::unified::write_diff`, on scripts built
//! from the input.

use rift_core::script::{apply, validate};
use rift_core::unified::{Label, Options, Timestamp, Verdict, write_diff};
use rift_core::{Edit, Hunk, LineNo, MemLines};

use crate::{apply_unified, config, fields, split_lines};

/// A script and the B it turns A into.
struct Built {
    hunks: Vec<Hunk>,
    b: Vec<u8>,
}

/// Turns `ops`, two bytes per edit, into a valid script over `a` and the B
/// it describes. The first byte picks the kind (low two bits), starts a new
/// hunk (bit 7) and marks that hunk approximate (bit 6); the second is the
/// length.
fn build(a: &[&[u8]], ops: &[u8], unterminated_b: bool) -> Built {
    // An unterminated last line of A can only end B, so only the closing
    // equal run below may copy it.
    let tail = usize::from(a.last().is_some_and(|l| !l.ends_with(b"\n")));
    let (mut ia, mut ib) = (0usize, 0u64);
    let mut b = Vec::new();
    let mut hunks = vec![Hunk::default()];
    let equal = |hunks: &mut Vec<Hunk>, b: &mut Vec<u8>, ia: &mut usize, ib: &mut u64, n| {
        hunks.last_mut().expect("one hunk").edits.push(Edit::Equal {
            a: LineNo(*ia as u64),
            b: LineNo(*ib),
            len: n as u64,
        });
        for line in &a[*ia..*ia + n] {
            b.extend_from_slice(line);
        }
        *ia += n;
        *ib += n as u64;
    };
    for &[k, n] in ops.as_chunks::<2>().0 {
        let n = usize::from(n);
        if k & 0x80 != 0 && !hunks.last().expect("one hunk").edits.is_empty() {
            hunks.push(Hunk::default());
        }
        if k & 0x40 != 0 {
            hunks.last_mut().expect("one hunk").approx = true;
        }
        match k & 3 {
            0 | 3 => {
                let want = if k & 3 == 0 { n % 8 + 1 } else { n };
                let len = want.min((a.len() - ia).saturating_sub(tail));
                if len > 0 {
                    equal(&mut hunks, &mut b, &mut ia, &mut ib, len);
                }
            }
            1 => {
                let len = (n % 8 + 1).min(a.len() - ia);
                if len > 0 {
                    hunks
                        .last_mut()
                        .expect("one hunk")
                        .edits
                        .push(Edit::Delete {
                            a: LineNo(ia as u64),
                            len: len as u64,
                        });
                    ia += len;
                }
            }
            _ => {
                let len = n % 4 + 1;
                hunks
                    .last_mut()
                    .expect("one hunk")
                    .edits
                    .push(Edit::Insert {
                        b: LineNo(ib),
                        len: len as u64,
                    });
                for _ in 0..len {
                    b.extend_from_slice(format!("ins {ib}\n").as_bytes());
                    ib += 1;
                }
            }
        }
    }
    let left = a.len() - ia;
    if left > 0 {
        equal(&mut hunks, &mut b, &mut ia, &mut ib, left);
    }
    let last = hunks.iter().rev().find_map(|h| h.edits.last());
    if unterminated_b && matches!(last, Some(Edit::Insert { .. })) {
        b.pop();
    }
    hunks.retain(|h| !h.edits.is_empty());
    Built { hunks, b }
}

/// A timestamp from the first 16 bytes of `bytes`: any second, any
/// offset.
fn timestamp(bytes: &[u8]) -> Option<Timestamp> {
    let secs = i64::from_le_bytes(bytes.get(..8)?.try_into().ok()?);
    let nanos = u32::from_le_bytes(bytes.get(8..12)?.try_into().ok()?) % 1_000_000_000;
    let utc_offset = i32::from_le_bytes(bytes.get(12..16)?.try_into().ok()?);
    Some(Timestamp {
        secs,
        nanos,
        utc_offset,
    })
}

/// Writes a script built from the input as a unified diff.
///
/// The first byte sets the context (0 to 7 lines, bits 0-2), a header
/// timestamp taken from the first 16 bytes of the ops (bit 3), an
/// unterminated last line of B (bit 4), and corrupt mode (bit 5).
///
/// Properties: the diff of a valid script, applied to A, gives B, with
/// verdict Different exactly when the script has a change. In corrupt
/// mode one edit is shifted, and the writer must accept the script
/// exactly when `script::validate` does.
///
/// # Panics
///
/// On any property violation.
pub fn check(data: &[u8]) {
    let (cfg, rest) = config(data);
    let [a, mut ops] = fields(rest);
    let mtime = if cfg & 8 != 0 {
        let t = timestamp(ops);
        ops = ops.get(16..).unwrap_or_default();
        t
    } else {
        None
    };
    let lines = split_lines(a);
    let Built { mut hunks, b } = build(&lines, ops, cfg & 16 != 0);
    let opts = Options {
        context: u64::from(cfg & 7),
        old: Label { name: b"a", mtime },
        new: Label { name: b"b", mtime },
    };
    let (la, lb) = (MemLines::new(a), MemLines::new(&b));
    let (len_a, len_b) = (lines.len() as u64, split_lines(&b).len() as u64);

    if cfg & 32 != 0 {
        corrupt(&mut hunks, ops);
        let valid = validate(&hunks, len_a, len_b).is_ok();
        let res = write_diff(&la, &lb, |i| Ok(hunks.get(i)), &opts, std::io::sink());
        assert_eq!(res.is_ok(), valid, "writer and validate disagree: {res:?}");
        return;
    }

    validate(&hunks, len_a, len_b).expect("built script is valid");
    let mut replay = Vec::new();
    apply(&la, &lb, &hunks, &mut replay).expect("built script replays");
    assert!(replay == b, "built script does not give b");

    let mut out = Vec::new();
    let verdict =
        write_diff(&la, &lb, |i| Ok(hunks.get(i)), &opts, &mut out).expect("writer failed");
    let changes = hunks
        .iter()
        .flat_map(|h| &h.edits)
        .any(|e| !matches!(e, Edit::Equal { .. }));
    assert_eq!(
        verdict == Verdict::Different,
        changes,
        "verdict {verdict:?}"
    );
    match apply_unified(a, &out) {
        Ok(got) => assert!(got == b, "the diff applied to a does not give b"),
        Err(e) => panic!("the diff does not apply to a: {e}"),
    }
}

/// Moves one edit, picked by the ops, off its place in the script.
fn corrupt(hunks: &mut [Hunk], ops: &[u8]) {
    let n: usize = hunks.iter().map(|h| h.edits.len()).sum();
    if n == 0 {
        return;
    }
    let pick = ops.iter().map(|&b| usize::from(b)).sum::<usize>();
    let Some(edit) = hunks.iter_mut().flat_map(|h| &mut h.edits).nth(pick % n) else {
        return;
    };
    match edit {
        Edit::Equal { a, .. } if pick % 3 == 0 => a.0 += 1,
        Edit::Equal { b, .. } | Edit::Insert { b, .. } if pick % 3 == 1 => b.0 += 1,
        Edit::Equal { len, .. } | Edit::Delete { len, .. } | Edit::Insert { len, .. } => {
            *len = if pick % 2 == 0 { *len + 1 } else { 0 };
        }
    }
}
