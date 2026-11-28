#![allow(clippy::cast_possible_truncation, reason = "test inputs are small")]

use std::sync::Arc;

use proptest::prelude::*;

use super::*;
use crate::unified::{Label, Options};
use crate::view::DiffView;

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

fn unified<A: Source + Send + ?Sized, B: Source + Send + ?Sized>(d: &Diff<A, B>) -> Vec<u8> {
    let mut out = Vec::new();
    d.write_unified(&opts(), &mut out).unwrap();
    out
}

/// The fresh diff of `a` and `b2`, and the one guided by the diff of `a`
/// and `b`: their unified output, and the path the guided one took.
fn both(a: &[u8], b: &[u8], b2: &[u8], mem: Bytes, threads: usize) -> (Vec<u8>, Vec<u8>, Path) {
    let a: Arc<[u8]> = a.into();
    let budget = Budget::new(mem);
    let old = Diff::new(Arc::clone(&a), Arc::<[u8]>::from(b), &budget, threads).unwrap();
    let segments = old.segments().to_vec();
    let starts = old.starts().to_vec();
    drop(old);
    let prev = Previous {
        segments: &segments,
        starts: &starts,
        len_a: a.len(),
        len_b: Bytes(b.len() as u64),
    };
    let b2: Arc<[u8]> = b2.into();
    let fresh = unified(&Diff::new(Arc::clone(&a), Arc::clone(&b2), &budget, threads).unwrap());
    assert_eq!(
        budget.available(Half::Skeleton),
        budget.capacity(Half::Skeleton)
    );
    let guide = Guide::new(&*a, &*b2, &prev, &budget).unwrap();
    let (d, path) = diff(
        a,
        b2,
        guide,
        &budget,
        threads,
        Config::default(),
        &Watch::new(),
    )
    .unwrap();
    (fresh, unified(&d), path)
}

fn rows(n: u64, f: impl Fn(u64) -> String) -> Vec<u8> {
    (0..n).flat_map(|i| f(i).into_bytes()).collect()
}

fn pair() -> (Vec<u8>, Vec<u8>) {
    let a = rows(30_000, |i| format!("row {i} {}\n", i % 17));
    let b = rows(30_000, |i| match i % 701 {
        3 => String::new(),
        5 => format!("changed {i}\n"),
        7 => format!("row {i} {}\nadded {i}\n", i % 17),
        // Same length, other bytes: a gap of equal shape that differs.
        11 => format!("rxw {i} {}\n", i % 17),
        _ => format!("row {i} {}\n", i % 17),
    });
    (a, b)
}

#[test]
fn an_append_is_guided_and_prints_what_a_fresh_diff_prints() {
    let (a, b) = pair();
    for tail in [
        rows(10, |i| format!("new {i}\n")),
        rows(3_000, |i| format!("new {i}\n")),
        // Lines that repeat lines of A and B, which takes their anchors
        // away in a fresh diff.
        rows(3_000, |i| format!("row {} {}\n", i * 9, (i * 9) % 17)),
        b[..b.len() / 3].to_vec(),
    ] {
        let mut b2 = b.clone();
        b2.extend_from_slice(&tail);
        let (fresh, guided, path) = both(&a, &b, &b2, Bytes::mib(64), 4);
        assert_eq!(
            String::from_utf8_lossy(&guided),
            String::from_utf8_lossy(&fresh)
        );
        let Path::Guided { confirmed, scanned } = path else {
            panic!("{path:?}");
        };
        assert!(confirmed.0 > scanned.0, "{path:?}");
    }
}

#[test]
fn a_wrong_guide_still_prints_what_a_fresh_diff_prints() {
    let (a, b) = pair();
    // Lines inserted near the start shift every range behind them.
    let mut b2 = b.clone();
    b2.splice(100..100, b"inserted\n".iter().copied());
    let (fresh, guided, _) = both(&a, &b, &b2, Bytes::mib(64), 2);
    assert_eq!(guided, fresh);
    // B' unrelated to B.
    let b2 = rows(20_000, |i| format!("other {i}\n"));
    let (fresh, guided, _) = both(&a, &b, &b2, Bytes::mib(64), 2);
    assert_eq!(guided, fresh);
}

#[test]
fn unterminated_last_lines_print_what_a_fresh_diff_prints() {
    let a = rows(5_000, |i| format!("row {i}\n"));
    let mut b = a.clone();
    b.extend_from_slice(b"open");
    let mut b2 = b.clone();
    b2.extend_from_slice(b" line\nmore\n");
    let (fresh, guided, _) = both(&a, &b, &b2, Bytes::mib(16), 2);
    assert_eq!(guided, fresh);
    let mut a = a;
    a.extend_from_slice(b"end");
    let (fresh, guided, path) = both(&a, &a, &b2, Bytes::mib(16), 2);
    assert_eq!(guided, fresh);
    assert!(matches!(path, Path::Guided { .. }), "{path:?}");
}

#[test]
fn a_small_budget_raises_the_stratum_as_a_fresh_diff_does() {
    let (a, b) = pair();
    let mut b2 = b.clone();
    b2.extend_from_slice(&rows(2_000, |i| format!("row {i} {}\n", i % 17)));
    for mem in [Bytes::mib(12), Bytes::mib(16), Bytes::mib(24)] {
        let (fresh, guided, path) = both(&a, &b, &b2, mem, 1);
        assert_eq!(guided, fresh, "{mem:?} {path:?}");
    }
}

#[test]
fn long_lines_print_what_a_fresh_diff_prints() {
    let mut a = rows(2_000, |i| format!("row {i}\n"));
    let at = a.len() / 2;
    a.splice(at..at, std::iter::repeat_n(b'y', 700_000).chain(*b"\n"));
    a.splice(10..10, std::iter::repeat_n(b'z', 300_000).chain(*b"\n"));
    let mut b = a.clone();
    b.splice(5000..5000, b"x\n".iter().copied());
    let mut b2 = b.clone();
    b2.extend_from_slice(&rows(100, |i| format!("tail {i}\n")));
    let (fresh, guided, path) = both(&a, &b, &b2, Bytes::mib(32), 2);
    assert_eq!(guided, fresh);
    assert!(matches!(path, Path::Guided { .. }), "{path:?}");
}

fn lines() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(0u8..24, 0..400).prop_map(|v| {
        v.into_iter()
            .flat_map(|x| format!("l{x}\n").into_bytes())
            .collect()
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn guided_output_is_the_fresh_output(
        a in lines(),
        b in lines(),
        tail in lines(),
        cut in 0usize..4096,
        mem in prop::sample::select(vec![11u64, 16, 64]),
    ) {
        let mut b2 = b.clone();
        b2.extend_from_slice(&tail);
        // Sometimes cut B' short or splice inside it: a guide that no
        // longer fits.
        if cut < b2.len() && cut % 3 == 0 {
            b2.truncate(cut);
        }
        let (fresh, guided, path) = both(&a, &b, &b2, Bytes::mib(mem), 1 + cut % 3);
        prop_assert_eq!(guided, fresh);
        prop_assert!(matches!(path, Path::Guided { .. }), "{:?}", path);
    }
}

#[test]
fn a_translated_guide_survives_an_insertion() {
    let (a, b) = pair();
    let mut b2 = b.clone();
    let at = b.len() / 5;
    let at = at + b[at..].iter().position(|&c| c == b'\n').unwrap() + 1;
    b2.splice(at..at, rows(40, |i| format!("inserted {i}\n")));
    let a: Arc<[u8]> = a.into();
    let budget = Budget::new(Bytes::mib(64));
    let old = Diff::new(Arc::clone(&a), Arc::<[u8]>::from(b.as_slice()), &budget, 2).unwrap();
    let (segments, starts) = (old.segments().to_vec(), old.starts().to_vec());
    let lines_b = old.len_b();
    drop(old);
    let prev = Previous {
        segments: &segments,
        starts: &starts,
        len_a: a.len(),
        len_b: Bytes(b.len() as u64),
    };
    let params = cdc::Params::new(cdc::DEFAULT_TARGET, b.len() as u64, lines_b);
    let old_t = cdc::table(b.as_slice(), params, &budget, Half::Refine).unwrap();
    let new_t = cdc::table(b2.as_slice(), params, &budget, Half::Refine).unwrap();
    let runs = cdc::match_runs(old_t.chunks(), new_t.chunks(), &budget, Half::Refine).unwrap();
    drop((old_t, new_t));
    let b2: Arc<[u8]> = b2.into();
    let fresh = unified(&Diff::new(Arc::clone(&a), Arc::clone(&b2), &budget, 2).unwrap());
    let guide = Guide::translated(&*a, &*b2, &prev, &runs, &budget).unwrap();
    drop(runs);
    let (d, path) = diff(a, b2, guide, &budget, 2, Config::default(), &Watch::new()).unwrap();
    assert_eq!(unified(&d), fresh);
    let Path::Guided { confirmed, scanned } = path else {
        panic!("{path:?}");
    };
    assert!(scanned.0 * 10 < confirmed.0, "{path:?}");
}

/// A saved result held in memory.
#[derive(Debug, Default)]
struct Mem(std::sync::Mutex<Vec<u8>>);

impl Source for Mem {
    fn len(&self) -> Bytes {
        Bytes(self.0.lock().unwrap().len() as u64)
    }

    fn read_at(&self, buf: &mut [u8], at: ByteOffset) -> std::io::Result<usize> {
        self.0.lock().unwrap().as_slice().read_at(buf, at)
    }
}

impl crate::saved::Sink for Mem {
    fn write_at(&self, buf: &[u8], at: ByteOffset) -> std::io::Result<()> {
        let mut v = self.0.lock().unwrap();
        let at = at.0 as usize;
        if v.len() < at + buf.len() {
            v.resize(at + buf.len(), 0);
        }
        v[at..at + buf.len()].copy_from_slice(buf);
        Ok(())
    }
}

#[test]
fn a_saved_result_guides_like_the_diff_it_came_from() {
    use crate::saved::{Identity, SavedFile, Writer, write_diff};
    let (a, b) = pair();
    let mut b2 = b.clone();
    b2.extend_from_slice(&rows(2_000, |i| format!("row {i} {}\n", i % 17)));
    let (a, b): (Arc<[u8]>, Arc<[u8]>) = (a.into(), b.into());
    let budget = Budget::new(Bytes::mib(32));
    let mem = Mem::default();
    {
        let sizes = [a.len(), b.len()];
        let writer = Writer::new(&mem, sizes, &budget).unwrap();
        let tables = writer.chunk_tables();
        let old = Diff::with_chunk_tables(Arc::clone(&a), b, &budget, 2, Config::default(), tables)
            .unwrap();
        let ident = |n: Bytes| Identity {
            size: n.0,
            ..Identity::default()
        };
        let files = [ident(sizes[0]), ident(sizes[1])];
        write_diff(
            &old,
            writer,
            files,
            Bytes::mib(32),
            &opts(),
            std::io::sink(),
        )
        .unwrap();
    }
    let b2: Arc<[u8]> = b2.into();
    let guide = {
        let saved = SavedFile::open(&mem, &budget).unwrap();
        let map = saved.map(&budget).unwrap();
        let sizes = &saved.meta().files;
        let prev = Previous {
            segments: &map.segments,
            starts: &map.starts,
            len_a: Bytes(sizes[0].size),
            len_b: Bytes(sizes[1].size),
        };
        Guide::new(&*a, &*b2, &prev, &budget).unwrap()
    };
    let fresh = unified(&Diff::new(Arc::clone(&a), Arc::clone(&b2), &budget, 2).unwrap());
    let (d, path) = diff(a, b2, guide, &budget, 2, Config::default(), &Watch::new()).unwrap();
    assert_eq!(unified(&d), fresh);
    assert!(matches!(path, Path::Guided { .. }), "{path:?}");
}
