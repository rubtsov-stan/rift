//! `pread` calls the extension's `hunk` requests cost. The only test in
//! this binary: the counter is process-wide.

use std::fmt::Write as _;
use std::time::{Duration, Instant};

use rift_core::{Bytes, DiffView, HunkState, LineNo};
use rift_session::test_hooks::reads;
use rift_session::{At, ExcerptRequest, Limits, SessionState, Sessions};

const LINES: u32 = 20_000;
const CHANGES: u32 = 40;

/// Short lines with a few longer ones, 40 single-line changes spread over
/// the file and a two-line insertion, like an edited log or source file.
fn pair(dir: &std::path::Path) -> (String, String) {
    let (mut a, mut b) = (String::new(), String::new());
    for i in 0..LINES {
        let line = if i % 97 == 0 {
            format!("row {i} {}\n", "x".repeat(300))
        } else {
            format!("row {i} value {}\n", i.wrapping_mul(2_654_435_761) % 1000)
        };
        a.push_str(&line);
        if i % (LINES / CHANGES) == 7 {
            writeln!(b, "edited {i}").unwrap();
        } else {
            b.push_str(&line);
        }
        if i % (LINES / CHANGES) == 400 {
            b.push_str("added one\nadded two\n");
        }
    }
    let path = |n: &str, s: &str| {
        let p = dir.join(n);
        std::fs::write(&p, s).unwrap();
        p.to_str().unwrap().to_owned()
    };
    (path("a", &a), path("b", &b))
}

#[test]
fn hunk_requests_read_little() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let s = Sessions::new(Limits {
        idle: Duration::from_secs(3600),
        ..Limits::default()
    });
    let session = s.open(&a, &b, Some(Bytes::mib(16))).unwrap().session;
    assert_eq!(session.wait(), SessionState::Ready);
    // What the extension sends after `open`: the summary, the first page of
    // segments, then the visible span of that page, so refinement settles.
    session.summary().unwrap();
    let total = session.differing().unwrap().count();
    session
        .diff()
        .unwrap()
        .set_visible(LineNo(0)..LineNo(u64::from(LINES)));
    let began = Instant::now();
    for i in 0..total {
        while matches!(session.hunk_state(i).unwrap(), HunkState::Pending) {
            assert!(began.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    // The user steps through the differences: one `hunk` per segment with
    // the default context, each followed by nothing else that reads.
    let before = reads();
    let mut answers = String::new();
    for i in 0..total {
        let mut req = ExcerptRequest::new(At::Segment(i));
        req.context = 3;
        let e = session.excerpt(&req).unwrap();
        writeln!(
            answers,
            "{} {:?} {:?} {:?} {:?}",
            e.segment, e.status, e.range, e.a, e.b
        )
        .unwrap();
    }
    let after = reads();
    let (n, bytes) = (after.0 - before.0, after.1 - before.1);
    let digest = answers.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, c| {
        (h ^ u64::from(c)).wrapping_mul(0x0100_0000_01b3)
    });
    eprintln!("segments {total} preads {n} bytes {bytes} digest {digest:016x}");

    assert_eq!(digest, 0xfb99_d25a_0cd5_ee2e, "answers differ from main");
    assert!(n <= 1000, "preads {n}");
}
