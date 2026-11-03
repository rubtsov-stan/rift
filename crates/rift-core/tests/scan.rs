//! Scanner, strata and index against an in-memory source.
//!
//! These live outside the crate's unit tests because every scan allocates
//! buffers, and the counting allocator's own tests need a quiet process.

use proptest::prelude::*;
use rift_core::index::IndexedLines;
use rift_core::scan::{ScanConfig, Scanned, scan};
use rift_core::{
    Budget, ByteOffset, Bytes, Error, Half, LineAccess, LineBuf, LineNo, MemLines, Sample, Source,
    strata,
};
use xxhash_rust::xxh3::xxh3_64;

fn config(level: u32, chunk: u64, threads: usize) -> ScanConfig {
    ScanConfig {
        level,
        sample_cap: 1 << 12,
        checkpoint_interval: 4,
        checkpoint_cap: 1 << 12,
        chunk: Bytes(chunk),
        threads,
    }
}

/// Samples of `level` computed line by line, the slow way.
fn expected(data: &[u8], level: u32) -> Vec<Sample> {
    let mem = MemLines::new(data);
    let mut offset = 0u64;
    let mut out = Vec::new();
    for i in 0..mem.line_count() {
        let line = mem.line(i).unwrap();
        let hash = xxh3_64(line);
        if strata::contains(level, hash) {
            out.push(Sample {
                hash,
                line: LineNo(i),
                offset: ByteOffset(offset),
            });
        }
        offset += line.len() as u64;
    }
    out
}

/// Scans and checks the line count, every sample and every checkpoint
/// against the slow version.
fn check(data: &[u8], cfg: &ScanConfig) -> Scanned {
    let got = scan(data, &Budget::new(Bytes::mib(64)), cfg).unwrap();
    assert_eq!(got.index().line_count(), MemLines::new(data).line_count());
    assert_eq!(got.samples(), expected(data, got.level()));
    let want = expected(data, 0);
    for c in got.index().checkpoints() {
        assert_eq!(c.offset, want[usize::try_from(c.line.0).unwrap()].offset);
    }
    got
}

fn lines(n: usize) -> Vec<u8> {
    (0..n)
        .flat_map(|i| format!("row {i} value {}\n", i * 7919 % 1000).into_bytes())
        .collect()
}

#[test]
fn counts_follow_wc_plus_unterminated_tail() {
    let cfg = config(0, 8, 2);
    assert_eq!(check(b"", &cfg).index().line_count(), 0);
    assert_eq!(check(b"\n", &cfg).index().line_count(), 1);
    assert_eq!(check(b"a\nb\n", &cfg).index().line_count(), 2);
    assert_eq!(check(b"a\nb", &cfg).index().line_count(), 2);
    assert_eq!(check(b"a\r\nb\r\n", &cfg).index().line_count(), 2);
    assert_eq!(check(b"\n\n\n", &cfg).index().line_count(), 3);
    assert_eq!(check(&lines(1000), &cfg).index().line_count(), 1000);
}

#[test]
fn terminator_is_part_of_the_hash() {
    let a = check(b"a\n", &config(0, 8, 1)).samples()[0].hash;
    let b = check(b"a\r\n", &config(0, 8, 1)).samples()[0].hash;
    let c = check(b"a", &config(0, 8, 1)).samples()[0].hash;
    assert_ne!(a, b);
    assert_ne!(a, c);
    assert_eq!(a, xxh3_64(b"a\n"));
}

#[test]
fn chunk_boundaries_do_not_change_the_result() {
    let data = lines(500);
    for chunk in [1, 2, 3, 7, 16, 64, 1000, 1 << 20] {
        for threads in [1, 2] {
            check(&data, &config(0, chunk, threads));
        }
    }
}

#[test]
fn higher_stratum_is_a_subset() {
    let data = lines(4000);
    let low = check(&data, &config(2, 128, 2));
    let high = check(&data, &config(3, 128, 2));
    assert!(!high.samples().is_empty());
    assert!(high.samples().len() < low.samples().len());
    assert!(high.samples().iter().all(|s| low.samples().contains(s)));
}

#[test]
fn overflow_raises_the_stratum_in_place() {
    let data = lines(4000);
    let mut cfg = config(0, 256, 2);
    cfg.sample_cap = 300;
    let got = check(&data, &cfg);
    assert!(got.level() > 0, "4000 lines cannot fit 300 samples at L=0");
    assert!(got.samples().len() <= 300);
}

#[test]
fn overflow_doubles_the_interval_in_place() {
    let data = lines(4000);
    let mut cfg = config(0, 8192, 2);
    cfg.checkpoint_cap = 64;
    let got = check(&data, &cfg);
    assert!(got.index().interval() > 4);
    assert!(got.index().checkpoints().len() <= 64);
    let gaps = got
        .index()
        .checkpoints()
        .windows(2)
        .map(|w| w[1].line - w[0].line);
    assert!(gaps.clone().all(|g| g <= got.index().interval()));
    assert!(gaps.clone().any(|g| g == got.index().interval()));
}

#[test]
fn line_longer_than_the_buffer_hashes_like_memory() {
    let mut data = vec![b'q'; 300 << 10];
    data.extend_from_slice(b"\nnext\n");
    data.extend(vec![b'z'; 200 << 10]);
    let got = check(&data, &config(0, 4 << 10, 2));
    assert_eq!(got.samples()[0].hash, xxh3_64(&data[..=(300 << 10)]));
    assert_eq!(got.samples()[2].hash, xxh3_64(&data[(300 << 10) + 6..]));
    assert_eq!(got.index().line_count(), 3);
}

#[test]
fn unterminated_last_line_of_exactly_the_buffer_length_counts() {
    // chunk 4096 plus the 64 KiB tail is the worker buffer.
    let buffer = 4096 + (64 << 10);
    for prefix in [&b""[..], b"a\n"] {
        let mut data = prefix.to_vec();
        data.extend(vec![b'q'; buffer]);
        let got = check(&data, &config(0, 4096, 2));
        assert_eq!(got.index().line_count(), 1 + u64::from(!prefix.is_empty()));
        assert_eq!(
            got.samples().last().unwrap().hash,
            xxh3_64(&data[prefix.len()..])
        );
    }
}

#[test]
fn for_pair_scans_under_a_small_budget() {
    let data = lines(3000);
    let budget = Budget::new(Bytes::mib(16));
    let (a, b) = ScanConfig::for_pair(Bytes(data.len() as u64), Bytes(10), &budget);
    let got_a = scan(&data[..], &budget, &a).unwrap();
    let got_b = scan(&b"one\n"[..], &budget, &b).unwrap();
    assert_eq!(got_a.samples(), expected(&data, got_a.level()));
    assert_eq!(got_b.index().line_count(), 1);
    assert!(budget.used(Half::Skeleton) <= budget.capacity(Half::Skeleton));
}

#[test]
fn thin_releases_budget() {
    let data = lines(2000);
    let budget = Budget::new(Bytes::mib(64));
    let mut got = scan(&data[..], &budget, &config(0, 1024, 2)).unwrap();
    let before = budget.used(Half::Skeleton);
    got.thin(4);
    assert_eq!(got.samples(), expected(&data, 4));
    assert!(budget.used(Half::Skeleton) < before);
    assert_eq!(budget.used(Half::Skeleton), got.held());
    drop(got);
    assert_eq!(budget.used(Half::Skeleton), Bytes(0));
}

#[test]
fn budget_is_returned_and_never_exceeded() {
    let data = lines(4000);
    let budget = Budget::new(Bytes::mib(64));
    let cfg = config(2, 1024, 2);
    let got = scan(&data[..], &budget, &cfg).unwrap();
    assert_eq!(budget.used(Half::Skeleton), got.held());
    assert!(budget.used(Half::Skeleton) <= budget.capacity(Half::Skeleton));
    assert_eq!(budget.used(Half::Refine), Bytes(0));
    assert_eq!(got.samples(), expected(&data, got.level()));
    drop(got);
    assert_eq!(budget.used(Half::Skeleton), Bytes(0));

    let tiny = Budget::new(Bytes(1 << 10));
    assert!(matches!(
        scan(&data[..], &tiny, &cfg),
        Err(Error::Budget(_))
    ));
    assert_eq!(tiny.used(Half::Skeleton), Bytes(0));
}

#[test]
fn index_reaches_every_line() {
    let data = lines(3000);
    let got = check(&data, &config(0, 512, 2));
    let budget = Budget::new(Bytes::mib(1));
    let access =
        IndexedLines::new(&data[..], got.index(), &budget, Half::Refine, Bytes(4096)).unwrap();
    let mem = MemLines::new(&data);
    let mut buf = LineBuf::new();
    for start in (0..3000u64).step_by(97) {
        access
            .read_lines(LineNo(start)..LineNo(start + 5), &mut buf)
            .unwrap();
        for (k, line) in buf.iter().enumerate() {
            assert_eq!(line, mem.line(start + k as u64).unwrap());
        }
    }
}

#[test]
fn read_errors_stop_every_worker() {
    struct Broken;
    impl Source for Broken {
        fn len(&self) -> Bytes {
            Bytes(256 << 10)
        }
        fn read_at(&self, _: &mut [u8], offset: ByteOffset) -> std::io::Result<usize> {
            Err(std::io::Error::other(format!("boom at {offset}")))
        }
    }
    let budget = Budget::new(Bytes::mib(64));
    assert!(matches!(
        scan(&Broken, &budget, &config(0, 4096, 2)),
        Err(Error::Io(_))
    ));
    assert_eq!(budget.used(Half::Skeleton), Bytes(0));
}

proptest! {
    /// Which original lines are sampled depends only on their content:
    /// inserting other lines anywhere leaves their membership alone.
    #[test]
    fn sampling_depends_only_on_line_content(
        original in prop::collection::vec("[a-c\r]{0,6}", 1..40),
        inserts in prop::collection::vec(("[a-d]{0,6}", 0..41usize), 0..10),
        chunk in 1u64..64,
    ) {
        let text = |ls: &[String]| ls.iter().flat_map(|l| format!("{l}\n").into_bytes()).collect::<Vec<u8>>();
        let mut edited = original.clone();
        for (line, at) in &inserts {
            edited.insert((*at).min(edited.len()), line.clone());
        }
        let cfg = config(1, chunk, 2);
        let before = check(&text(&original), &cfg);
        let after = check(&text(&edited), &cfg);
        let hashes = |s: &Scanned| s.samples().iter().map(|x| x.hash).collect::<Vec<_>>();
        let mut want = hashes(&before);
        want.extend(inserts.iter().map(|(l, _)| xxh3_64(format!("{l}\n").as_bytes())).filter(|&h| strata::contains(1, h)));
        want.sort_unstable();
        let mut got = hashes(&after);
        got.sort_unstable();
        prop_assert_eq!(got, want);
    }
}
