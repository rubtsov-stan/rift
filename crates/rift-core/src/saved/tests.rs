use std::io;
use std::sync::{Arc, Mutex, PoisonError};

use proptest::prelude::*;
use xxhash_rust::xxh3::xxh3_64;

use super::*;
use crate::budget::{Budget, Half};
use crate::diff::{Config, Diff};
use crate::edit::{Edit, Hunk};
use crate::index::{Checkpoint, LineIndex};
use crate::lines::LineAccess;
use crate::moves::Moves;
use crate::segment::{Segment, SegmentStatus};
use crate::types::{Bytes, LineNo};
use crate::unified::{Label, Options};
use crate::view::{DiffView, HunkState};

/// A result held in memory.
#[derive(Debug, Default)]
pub(crate) struct MemSink(Mutex<Vec<u8>>);

impl MemSink {
    pub(crate) fn bytes(&self) -> Vec<u8> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Source for MemSink {
    fn len(&self) -> Bytes {
        Bytes(self.0.lock().unwrap_or_else(PoisonError::into_inner).len() as u64)
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_slice()
            .read_at(buf, offset)
    }
}

impl Sink for MemSink {
    fn write_at(&self, buf: &[u8], offset: ByteOffset) -> io::Result<()> {
        let mut v = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let at = usize::try_from(offset.0).unwrap();
        if v.len() < at + buf.len() {
            v.resize(at + buf.len(), 0);
        }
        v[at..at + buf.len()].copy_from_slice(buf);
        Ok(())
    }
}

fn opts() -> Options<'static> {
    let label = |name: &'static str| Label {
        name: name.as_bytes(),
        mtime: None,
    };
    Options {
        context: 3,
        old: label("a"),
        new: label("b"),
    }
}

fn ident(path: &str, size: usize) -> Identity {
    Identity {
        path: path.into(),
        canonical: format!("/abs/{path}").into(),
        size: size as u64,
        mtime_secs: -5,
        mtime_nanos: 7,
        dev: 1,
        ino: 2,
    }
}

/// Saves the diff of `a` and `b` under `mem`; returns the result, the
/// unified output and the live diff's scripts.
fn save(a: &[u8], b: &[u8], mem: Bytes) -> (Vec<u8>, Vec<u8>, Vec<Option<Hunk>>) {
    let budget = Budget::new(mem);
    let sink = MemSink::default();
    let w = Writer::new(
        &sink,
        [Bytes(a.len() as u64), Bytes(b.len() as u64)],
        &budget,
    )
    .unwrap();
    let tables = w.chunk_tables();
    let (sa, sb): (Arc<[u8]>, Arc<[u8]>) = (a.into(), b.into());
    let diff = Diff::with_chunk_tables(sa, sb, &budget, 2, Config::default(), tables).unwrap();
    let mut out = Vec::new();
    write_diff(
        &diff,
        w,
        [ident("a", a.len()), ident("b", b.len())],
        mem,
        &opts(),
        &mut out,
    )
    .unwrap();
    let hunks = (0..diff.segments().len())
        .map(|i| match diff.hunk(i) {
            HunkState::Ready(h) | HunkState::Approx(h) => Some((*h).clone()),
            HunkState::Pending => None,
        })
        .collect();
    let mut plain = Vec::new();
    diff.write_unified(&opts(), &mut plain).unwrap();
    assert_eq!(out, plain, "saving changes the output");
    (sink.bytes(), out, hunks)
}

fn lines(n: usize, f: impl Fn(usize) -> String) -> Vec<u8> {
    (0..n).flat_map(|i| f(i).into_bytes()).collect()
}

#[test]
fn a_saved_diff_reopens_with_the_same_map_scripts_and_text() {
    let a = lines(3000, |i| format!("row {i} value {}\n", i * 7));
    let b = lines(3000, |i| {
        if i % 97 == 5 {
            format!("row {i} changed\n")
        } else {
            format!("row {i} value {}\n", i * 7)
        }
    });
    let mem = Bytes::mib(32);
    let (result, _, hunks) = save(&a, &b, mem);
    let budget = Budget::new(mem);
    let file = SavedFile::open(&result[..], &budget).unwrap();
    assert_eq!(file.version(), (MAJOR, MINOR));
    let meta = file.meta().clone();
    assert_eq!(meta.files, [ident("a", a.len()), ident("b", b.len())]);
    assert_eq!(meta.mem, mem.0);
    assert_eq!(meta.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(meta.lines, [3000, 3000]);
    let view = SavedDiff::open(file, &a[..], &b[..], &budget).unwrap();
    let live = {
        let (sa, sb): (Arc<[u8]>, Arc<[u8]>) = (a.as_slice().into(), b.as_slice().into());
        Diff::new(sa, sb, &Budget::new(mem), 2).unwrap()
    };
    assert_eq!(view.segments(), live.segments());
    for (i, h) in hunks.iter().enumerate() {
        let got = view.try_hunk(i).unwrap().map(|h| (*h).clone());
        assert_eq!(got.as_ref(), h.as_ref(), "segment {i}");
    }
    let mut buf = crate::LineBuf::new();
    view.lines_b()
        .read_lines(LineNo(5)..LineNo(6), &mut buf)
        .unwrap();
    assert_eq!(buf.line(0), b"row 5 changed\n");
    assert!(!view.changed());
}

#[test]
fn chunk_tables_hash_every_mebibyte_of_both_files() {
    // Three scan chunks and a bit, so hash chunks cross scan chunks.
    let a = lines(170_000, |i| format!("{i:08} some text to fill the line\n"));
    let mut b = a.clone();
    b.extend_from_slice(b"tail\n");
    let (result, _, _) = save(&a, &b, Bytes::mib(64));
    let file = SavedFile::open(&result[..], &Budget::new(Bytes::mib(64))).unwrap();
    for (side, data) in [(0, &a), (1, &b)] {
        let (table, _held) = file
            .chunk_table(side, &Budget::new(Bytes::mib(64)), Half::Refine)
            .unwrap();
        let want: Vec<u64> = data.chunks(1 << CHUNK_SHIFT).map(xxh3_64).collect();
        assert!(want.len() > 4);
        assert_eq!(table, want, "side {side}");
    }
}

#[test]
fn chunk_hashes_do_not_depend_on_scan_chunks_or_threads() {
    use crate::scan::{Meter, ScanConfig, Watch, scan_metered};
    let data = lines(5000, |i| format!("line {i} {}\n", "x".repeat(i % 37)));
    let want: Vec<u8> = data
        .chunks(4096)
        .flat_map(|c| xxh3_64(c).to_le_bytes())
        .collect();
    // Scan chunks of 3000, 7777 and 1 MiB bytes against hash chunks of
    // 4 KiB: most hash chunks cross a scan chunk's end.
    for (chunk, interval, threads) in [
        (3000, 1000, 1),
        (3000, 1000, 4),
        (7777, 1111, 3),
        (1 << 20, 1024, 2),
    ] {
        let sink = MemSink::default();
        sink.write_at(&vec![0; want.len()], ByteOffset(0)).unwrap();
        let cfg = ScanConfig {
            level: 0,
            sample_cap: 1 << 20,
            checkpoint_interval: Bytes(interval),
            checkpoint_cap: 1 << 20,
            chunk: Bytes(chunk),
            buffer: Bytes(512),
            threads,
        };
        let watch = Watch::new();
        let meter = Meter::hashing(&watch, 0, Some(ChunkTable::new(&sink, 0, 12)));
        scan_metered(&data[..], &Budget::new(Bytes::mib(64)), &cfg, &meter).unwrap();
        assert!(sink.bytes() == want, "chunk {chunk}, {threads} threads");
    }
}

#[test]
fn a_read_from_a_changed_file_fails_and_keeps_failing() {
    let a = lines(100, |i| format!("a {i}\n"));
    let b = lines(100, |i| format!("b {i}\n"));
    let (result, _, _) = save(&a, &b, Bytes::mib(32));
    let budget = Budget::new(Bytes::mib(32));
    let file = SavedFile::open(&result[..], &budget).unwrap();
    let mut b2 = b.clone();
    b2[3] = b'X';
    let view = SavedDiff::open(file, &a[..], &b2[..], &budget).unwrap();
    let mut buf = crate::LineBuf::new();
    let err = view
        .lines_b()
        .read_lines(LineNo(50)..LineNo(51), &mut buf)
        .unwrap_err();
    assert!(err.to_string().contains("b changed"), "{err}");
    assert!(view.changed());
    view.lines_a()
        .read_lines(LineNo(0)..LineNo(1), &mut buf)
        .unwrap();
    assert!(
        view.lines_b()
            .read_lines(LineNo(50)..LineNo(51), &mut buf)
            .is_err()
    );
}

#[test]
fn a_file_of_another_length_is_refused() {
    let a = lines(10, |i| format!("a {i}\n"));
    let (result, _, _) = save(&a, &a, Bytes::mib(32));
    let budget = Budget::new(Bytes::mib(32));
    let file = SavedFile::open(&result[..], &budget).unwrap();
    let err = SavedDiff::open(file, &a[..], &a[1..], &budget).unwrap_err();
    assert!(err.to_string().contains("the file changed"), "{err}");
}

/// A small result written straight through the writer, with `extra`
/// sections added before `finish`.
fn tiny(extra: &[(u16, bool)]) -> Vec<u8> {
    let budget = Budget::new(Bytes::mib(8));
    let sink = MemSink::default();
    let mut w = Writer::new(&sink, [Bytes(4), Bytes(4)], &budget).unwrap();
    let seg = Segment {
        a: LineNo(0)..LineNo(2),
        b: LineNo(0)..LineNo(2),
        status: SegmentStatus::Equal,
    };
    w.push(&seg, [4, 4], None).unwrap();
    for &(k, critical) in extra {
        w.section(k, critical, b"later").unwrap();
    }
    let ix = LineIndex::new(
        vec![Checkpoint {
            line: LineNo(0),
            offset: ByteOffset(0),
        }],
        2,
        Bytes(1024),
    );
    w.finish(
        [ident("a", 4), ident("b", 4)],
        Bytes::mib(8),
        [&ix, &ix],
        &Moves::off(),
    )
    .unwrap();
    sink.bytes()
}

#[test]
fn unknown_sections_are_skipped_unless_critical() {
    let budget = Budget::new(Bytes::mib(8));
    let ok = tiny(&[(kind::CHUNKS_CDC_A, false), (99, false)]);
    let file = SavedFile::open(&ok[..], &budget).unwrap();
    assert!(file.section(99).is_some());
    file.map(&budget).unwrap();
    let refused = tiny(&[(99, true)]);
    let err = SavedFile::open(&refused[..], &budget).unwrap_err();
    assert!(err.to_string().contains("section type 99"), "{err}");
}

#[test]
fn an_unknown_major_and_a_missing_footer_are_refused() {
    let budget = Budget::new(Bytes::mib(8));
    let mut r = tiny(&[]);
    r[8] = 2;
    let hash = xxh3_64(&r[..56]);
    r[56..64].copy_from_slice(&hash.to_le_bytes());
    let err = SavedFile::open(&r[..], &budget).unwrap_err();
    assert!(err.to_string().contains("format 2.0"), "{err}");
    let r = tiny(&[]);
    let err = SavedFile::open(&r[..r.len() - 1], &budget).unwrap_err();
    assert!(err.to_string().contains("incomplete"), "{err}");
}

/// Every check a reader can run on `bytes`, as a fuzz target would.
fn read_all(bytes: &[u8]) -> Result<()> {
    let budget = Budget::new(Bytes::mib(16));
    let file = SavedFile::open(bytes, &budget)?;
    let map = file.map(&budget)?;
    file.line_indexes(&budget)?;
    file.moves(&budget)?;
    file.chunk_table(0, &budget, Half::Refine)?;
    let mut buf = vec![0; usize::try_from(map.max_block).unwrap()];
    for j in 0..map.blocks.len() {
        file.block_hunks(&map, j, &mut buf)?;
    }
    Ok(())
}

/// A drawn segment: status, lines of A and B, and its script's reason and
/// raw edits, if it has one.
type Drawn = (u8, u64, u64, Option<(Option<u8>, Vec<(u8, u64)>)>);

/// A segment and its script, drawn so that they are consistent.
fn segment() -> impl Strategy<Value = Drawn> {
    (
        0u8..3,
        0u64..20,
        0u64..20,
        any::<bool>(),
        proptest::option::of(0u8..3),
        proptest::collection::vec((0u8..3, 0u64..5), 0..8),
    )
        .prop_map(|(status, la, lb, refined, reason, edits)| {
            let (la, lb) = if status == 0 {
                (la.max(1), la.max(1))
            } else if la + lb == 0 {
                (1, 0)
            } else {
                (la, lb)
            };
            let script = (status != 0 && refined).then_some((reason, edits));
            (status, la, lb, script)
        })
}

/// Builds a script for a segment of `la` and `lb` lines from `raw`,
/// fixed up so it covers the segment exactly.
fn script(start: (u64, u64), la: u64, lb: u64, raw: &[(u8, u64)]) -> Vec<Edit> {
    let (mut a, mut b) = start;
    let (ea, eb) = (start.0 + la, start.1 + lb);
    let mut out = Vec::new();
    for &(k, len) in raw {
        match k {
            0 => {
                let len = len.min(ea - a).min(eb - b);
                out.push(Edit::Equal {
                    a: LineNo(a),
                    b: LineNo(b),
                    len,
                });
                a += len;
                b += len;
            }
            1 => {
                let len = len.min(ea - a);
                out.push(Edit::Delete { a: LineNo(a), len });
                a += len;
            }
            _ => {
                let len = len.min(eb - b);
                out.push(Edit::Insert { b: LineNo(b), len });
                b += len;
            }
        }
    }
    if a < ea {
        out.push(Edit::Delete {
            a: LineNo(a),
            len: ea - a,
        });
    }
    if b < eb {
        out.push(Edit::Insert {
            b: LineNo(b),
            len: eb - b,
        });
    }
    out
}

/// A result with the segments, their byte starts and scripts, and meta.
type Written = (
    Vec<u8>,
    Vec<Segment>,
    Vec<[ByteOffset; 2]>,
    Vec<Option<Hunk>>,
    Meta,
);

/// Writes `segs` and returns the result with what it holds.
fn written(segs: &[Drawn]) -> Written {
    let budget = Budget::new(Bytes::mib(8));
    let sink = MemSink::default();
    let (mut la, mut lb, mut ba, mut bb) = (0, 0, 0, 0);
    let mut plan = Vec::new();
    for (status, x, y, s) in segs {
        let seg = Segment {
            a: LineNo(la)..LineNo(la + x),
            b: LineNo(lb)..LineNo(lb + y),
            status: [
                SegmentStatus::Equal,
                SegmentStatus::Differs,
                SegmentStatus::Approx,
            ][*status as usize],
        };
        // Two bytes per line, three for a line of B, so equal ranges differ
        // from unequal ones in bytes too.
        let bytes = if *status == 0 {
            [2 * x, 2 * x]
        } else {
            [2 * x, 3 * y]
        };
        let hunk = s.as_ref().map(|(reason, raw)| Hunk {
            edits: script((la, lb), *x, *y, raw),
            approx: reason.map(|r| ApproxReason::ALL[r as usize]),
        });
        plan.push((seg, bytes, hunk, [ByteOffset(ba), ByteOffset(bb)]));
        (la, lb, ba, bb) = (la + x, lb + y, ba + bytes[0], bb + bytes[1]);
    }
    let mut w = Writer::new(&sink, [Bytes(ba), Bytes(bb)], &budget).unwrap();
    for (seg, bytes, hunk, _) in &plan {
        w.push(seg, *bytes, hunk.as_ref()).unwrap();
    }
    let index = |lines: u64| {
        let cps = if lines > 0 {
            vec![Checkpoint {
                line: LineNo(0),
                offset: ByteOffset(0),
            }]
        } else {
            vec![]
        };
        LineIndex::new(cps, lines, Bytes(1024))
    };
    let (ia, ib) = (index(la), index(lb));
    let files = [
        ident("x", usize::try_from(ba).unwrap()),
        ident("y", usize::try_from(bb).unwrap()),
    ];
    w.finish(
        files.clone(),
        Bytes::mib(8),
        [&ia, &ib],
        &Moves::not_searched(),
    )
    .unwrap();
    let mut meta = Meta {
        files,
        mem: Bytes::mib(8).0,
        moves: true,
        version: env!("CARGO_PKG_VERSION").into(),
        lines: [la, lb],
        ..Meta::default()
    };
    for (seg, _, hunk, _) in &plan {
        meta.segments[seg.status as usize] += 1;
        if let Some(h) = hunk.as_ref().filter(|_| seg.status != SegmentStatus::Equal) {
            meta.refined += 1;
            if let Some(r) = h.approx {
                meta.approx[r as usize] += 1;
            }
        }
    }
    let hunks = plan
        .iter()
        .map(|(s, _, h, _)| h.clone().filter(|_| s.status != SegmentStatus::Equal))
        .collect();
    let starts = plan.iter().map(|p| p.3).collect();
    (
        sink.bytes(),
        plan.into_iter().map(|p| p.0).collect(),
        starts,
        hunks,
        meta,
    )
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn write_then_read_gives_back_the_map_scripts_and_meta(segs in proptest::collection::vec(segment(), 0..300)) {
        let (result, segments, starts, hunks, meta) = written(&segs);
        let budget = Budget::new(Bytes::mib(8));
        let file = SavedFile::open(&result[..], &budget).unwrap();
        prop_assert_eq!(file.meta(), &meta);
        let map = file.map(&budget).unwrap();
        prop_assert_eq!(&map.segments, &segments);
        prop_assert_eq!(&map.starts, &starts);
        let mut buf = vec![0; usize::try_from(map.max_block).unwrap()];
        let mut got = Vec::new();
        for j in 0..map.blocks.len() {
            got.extend(file.block_hunks(&map, j, &mut buf).unwrap().into_iter().map(|h| h.map(|h| (*h).clone())));
        }
        prop_assert_eq!(got, hunks);
    }

    #[test]
    fn random_bytes_are_refused_without_a_panic(bytes in proptest::collection::vec(any::<u8>(), 0..4096)) {
        prop_assert!(read_all(&bytes).is_err());
    }

    #[test]
    fn mutated_results_never_panic(
        segs in proptest::collection::vec(segment(), 0..60),
        flips in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..6),
        cut in any::<prop::sample::Index>(),
        truncate in any::<bool>(),
    ) {
        let (mut result, ..) = written(&segs);
        for (at, v) in flips {
            let i = at.index(result.len());
            result[i] ^= v.max(1);
        }
        if truncate {
            result.truncate(cut.index(result.len()));
        }
        // Errors or not, as long as it returns.
        let _ = read_all(&result);
    }
}
