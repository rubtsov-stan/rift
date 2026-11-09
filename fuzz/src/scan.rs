//! The line scanner: `rift_core::scan::scan` and the index it builds.

use rift_core::index::IndexedLines;
use rift_core::scan::{ScanConfig, scan};
use rift_core::{Budget, ByteOffset, Bytes, Error, Half, LineAccess, LineBuf, LineNo, strata};

use crate::split_lines;

const CHUNKS: [u64; 8] = [1, 2, 3, 7, 16, 64, 4096, 1 << 20];
const CAPS: [usize; 4] = [1, 2, 16, 1 << 16];
const SCRATCH: [u64; 4] = [1, 7, 64, 4096];

/// Serves `offset % buf.len() + 1` bytes per read: short reads, as the
/// `Source` contract allows.
struct Short<'a>(&'a [u8]);

impl rift_core::Source for Short<'_> {
    fn len(&self) -> Bytes {
        Bytes(self.0.len() as u64)
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> std::io::Result<usize> {
        let step = offset.0 % buf.len().max(1) as u64 + 1;
        let n = usize::try_from(step).unwrap_or(usize::MAX).min(buf.len());
        rift_core::Source::read_at(self.0, &mut buf[..n], offset)
    }
}

/// Scans the input with a configuration taken from its first two bytes
/// (chunk size, threads, sample and checkpoint caps, reader scratch) and
/// checks the result against a plain split of the bytes.
///
/// # Panics
///
/// On any property violation; that is the finding.
pub fn check(data: &[u8]) {
    let (c0, c1, text) = match data {
        [c0, c1, text @ ..] => (*c0, *c1, text),
        _ => return,
    };
    let budget = Budget::new(Bytes::mib(64));
    let cfg = ScanConfig {
        level: u32::from(c1 >> 6),
        sample_cap: CAPS[usize::from(c0 >> 5 & 3)],
        checkpoint_interval: u64::from(c1 & 7) + 1,
        checkpoint_cap: CAPS[usize::from(c1 >> 3 & 3)].max(2),
        chunk: Bytes(CHUNKS[usize::from(c0 & 7)]),
        threads: usize::from(c0 >> 3 & 3) + 1,
    };
    let scanned = match scan(text, &budget, &cfg) {
        Ok(s) => s,
        // Caps this small may leave no stratum or interval that fits.
        Err(Error::Budget(_)) => return,
        Err(e) => panic!("scan failed: {e}"),
    };
    let lines = split_lines(text);
    let starts: Vec<u64> = lines
        .iter()
        .scan(0u64, |at, l| {
            let s = *at;
            *at += l.len() as u64;
            Some(s)
        })
        .collect();
    let index = scanned.index();
    assert_eq!(index.line_count(), lines.len() as u64, "line count");

    let cps = index.checkpoints();
    if !lines.is_empty() {
        assert_eq!(
            cps.first().map(|c| c.line),
            Some(LineNo(0)),
            "first checkpoint"
        );
    }
    for w in cps.windows(2) {
        assert!(w[0].line < w[1].line, "checkpoints out of order");
        assert!(
            w[1].line.0 - w[0].line.0 <= index.interval(),
            "checkpoint gap"
        );
    }
    for c in cps {
        assert_eq!(starts.get(c.line.0 as usize), Some(&c.offset.0), "{c:?}");
    }

    let level = scanned.level();
    for w in scanned.samples().windows(2) {
        assert!(w[0].line < w[1].line, "samples out of order");
    }
    for s in scanned.samples() {
        assert!(
            strata::contains(level, s.hash),
            "{s:?} outside stratum {level}"
        );
        assert_eq!(starts.get(s.line.0 as usize), Some(&s.offset.0), "{s:?}");
    }

    // One chunk, one thread and roomy caps hash every line whole: the
    // samples must not depend on where the chunks were cut.
    let reference = ScanConfig {
        level: cfg.level,
        sample_cap: 1 << 16,
        checkpoint_interval: 1,
        checkpoint_cap: 1 << 16,
        chunk: Bytes(text.len() as u64 + 1),
        threads: 1,
    };
    let mut whole = scan(text, &budget, &reference).expect("reference scan");
    let common = whole.level().max(level);
    whole.thin(common);
    let mut ours = scanned.samples().to_vec();
    strata::thin(&mut ours, common);
    assert_eq!(
        ours,
        whole.samples(),
        "samples differ from a one-chunk scan"
    );

    for scratch in [
        SCRATCH[usize::from(c0 >> 7)],
        SCRATCH[2 + usize::from(c1 >> 5 & 1)],
    ] {
        read_back(text, index, &lines, &budget, scratch);
        read_back(&Short(text), index, &lines, &budget, scratch);
    }
}

/// Reads every line through an index-backed reader with `scratch` bytes
/// and compares it with the plain split.
fn read_back<S: rift_core::Source + ?Sized>(
    text: &S,
    index: &rift_core::index::LineIndex,
    lines: &[&[u8]],
    budget: &Budget,
    scratch: u64,
) {
    let reader = IndexedLines::new(text, index, budget, Half::Refine, Bytes(scratch))
        .expect("reader scratch fits");
    assert_eq!(reader.line_count(), lines.len() as u64);
    let mut buf = LineBuf::new();
    for (i, want) in lines.iter().enumerate() {
        let n = LineNo(i as u64);
        match reader.read_lines(n..LineNo(n.0 + 1), &mut buf) {
            Ok(()) => assert_eq!(buf.line(0), *want, "line {i}"),
            Err(Error::LineTooLong { .. }) => {
                assert!(want.len() as u64 > scratch, "line {i} refused but fits");
            }
            Err(e) => panic!("line {i}: {e}"),
        }
        let len = reader
            .read_line_prefix(n, Bytes(5), &mut buf)
            .expect("prefix");
        assert_eq!(len.0, want.len() as u64, "length of line {i}");
        assert_eq!(
            buf.line(0),
            &want[..want.len().min(5)],
            "prefix of line {i}"
        );
        let mut got = Vec::new();
        loop {
            let last = reader
                .read_line_part(n, Bytes(got.len() as u64), Bytes(3), &mut buf)
                .expect("part");
            got.extend_from_slice(buf.line(0));
            if last {
                break;
            }
            assert!(got.len() < want.len(), "line {i} streams past its end");
        }
        assert_eq!(got, *want, "line {i} in pieces");
    }
}
