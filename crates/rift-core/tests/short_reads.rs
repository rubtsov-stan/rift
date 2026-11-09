//! A `Source` may return fewer bytes than asked. The indexed reader must
//! give the same lines for short reads as for full ones.

use proptest::prelude::*;
use rift_core::index::IndexedLines;
use rift_core::scan::{ScanConfig, scan};
use rift_core::{
    Budget, ByteOffset, Bytes, Error, Half, LineAccess, LineBuf, LineNo, MemLines, Source,
};

/// Serves `offset % buf.len() + 1` bytes per read.
struct Short<'a>(&'a [u8]);

impl Source for Short<'_> {
    fn len(&self) -> Bytes {
        Bytes(self.0.len() as u64)
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> std::io::Result<usize> {
        let step = offset.0 % buf.len().max(1) as u64 + 1;
        let n = usize::try_from(step).unwrap_or(usize::MAX).min(buf.len());
        self.0.read_at(&mut buf[..n], offset)
    }
}

#[test]
fn a_long_line_skipped_by_short_reads_is_still_too_long_when_read() {
    let text: &[u8] = b"\naa\na\naazzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\n\n";
    let src = Short(text);
    let budget = Budget::new(Bytes::mib(64));
    let cfg = ScanConfig {
        level: 0,
        sample_cap: 1 << 10,
        checkpoint_interval: 3,
        checkpoint_cap: 1 << 10,
        chunk: Bytes(1 << 20),
        threads: 1,
    };
    let scanned = scan(&src, &budget, &cfg).unwrap();
    let reader =
        IndexedLines::new(&src, scanned.index(), &budget, Half::Refine, Bytes(13)).unwrap();
    let mut buf = LineBuf::new();
    reader.read_lines(LineNo(4)..LineNo(5), &mut buf).unwrap();
    assert_eq!(buf.line(0), b"\n");
    match reader.read_lines(LineNo(3)..LineNo(4), &mut buf) {
        Err(Error::LineTooLong { line, .. }) => assert_eq!(line, LineNo(3)),
        Ok(()) => panic!(
            "line 3 came back as {:?}, cut from a 38-byte line",
            String::from_utf8_lossy(buf.line(0))
        ),
        Err(e) => panic!("{e}"),
    }
}

proptest! {
    /// Any order of reads, through a source that returns short reads, gives
    /// the lines of a plain split, or `LineTooLong` for a line longer than
    /// the scratch buffer.
    #[test]
    fn reads_in_any_order_match_a_plain_split(
        text in prop::collection::vec(
            prop_oneof![
                8 => Just(b"a".to_vec()),
                6 => Just(b"\n".to_vec()),
                1 => (1usize..60).prop_map(|n| vec![b'z'; n]),
            ],
            0..80,
        ).prop_map(|v| v.concat()),
        interval in 1u64..6,
        scratch in 1u64..40,
        order in prop::collection::vec(0u64..1000, 0..40),
    ) {
        let src = Short(&text);
        let budget = Budget::new(Bytes::mib(64));
        let cfg = ScanConfig {
            level: 0,
            sample_cap: 1 << 10,
            checkpoint_interval: interval,
            checkpoint_cap: 1 << 10,
            chunk: Bytes(1 << 20),
            threads: 1,
        };
        let scanned = scan(&src, &budget, &cfg).unwrap();
        let reader = IndexedLines::new(&src, scanned.index(), &budget, Half::Refine, Bytes(scratch)).unwrap();
        let mem = MemLines::new(&text);
        let n = mem.line_count();
        let mut buf = LineBuf::new();
        for i in order.into_iter().filter(|_| n > 0).map(|i| i % n) {
            let want = mem.line(i).unwrap();
            match reader.read_lines(LineNo(i)..LineNo(i + 1), &mut buf) {
                Ok(()) => prop_assert_eq!(buf.line(0), want, "line {}", i),
                Err(Error::LineTooLong { .. }) => prop_assert!(want.len() as u64 > scratch),
                Err(e) => panic!("line {i}: {e}"),
            }
        }
    }
}
