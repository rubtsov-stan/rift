//! A file rewritten in place after bytes of it were read is never answered
//! from those bytes: the reader keeps what its last read brought in, and the
//! next excerpt can be served from it without touching the file. The only
//! test in this binary: the read counter is process-wide.

use std::io::Write as _;
use std::time::Duration;

use rift_core::Bytes;
use rift_session::test_hooks::reads;
use rift_session::{At, ExcerptRequest, Limits, SessionState, Sessions};

/// The protocol's `changed` code.
const CHANGED: i32 = -32009;

fn numbered() -> String {
    let bytes = (1..=40).flat_map(|i| format!("line {i}\n").into_bytes());
    String::from_utf8(bytes.collect()).unwrap()
}

/// Writes `bytes` over the file at `path` in place, keeping its inode, and
/// sets its modification time a minute later than it was.
fn rewrite(path: &str, bytes: &str) {
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .unwrap();
    let mtime = f.metadata().unwrap().modified().unwrap();
    f.write_all(bytes.as_bytes()).unwrap();
    f.set_modified(mtime + Duration::from_secs(60)).unwrap();
}

fn rewrite_after_a_hunk(line15: &str) {
    let dir = tempfile::tempdir().unwrap();
    let a_text = numbered();
    let path = |n: &str, s: &str| {
        let p = dir.path().join(n);
        std::fs::write(&p, s).unwrap();
        p.to_str().unwrap().to_owned()
    };
    let (a, b) = (
        path("a", &a_text),
        path("b", &a_text.replace("line 10\n", "changed 10\n")),
    );
    let s = Sessions::new(Limits {
        idle: Duration::from_secs(3600),
        ..Limits::default()
    });
    let session = s.open(&a, &b, Some(Bytes::mib(16))).unwrap().session;
    assert_eq!(session.wait(), SessionState::Ready);

    // The hunk reads lines 7 to 13 of each file, so each reader holds the
    // bytes after line 13, line 15 among them.
    let mut hunk = ExcerptRequest::new(At::Segment(1));
    hunk.context = 3;
    session.excerpt(&hunk).unwrap();
    // Refinement of the segments is done with the files before the rewrite.
    std::thread::sleep(Duration::from_millis(300));

    rewrite(&a, &a_text.replace("line 15\n", &format!("{line15}\n")));

    // The lines after the hunk, from the first one the held bytes reach.
    let mut after = ExcerptRequest::new(At::LineA(rift_core::LineNo(13)));
    after.context = 0;
    let before = reads();
    let answer = session.excerpt(&after);
    let read = reads().0 - before.0;
    assert_eq!(read, 0, "the second hunk was not served from held bytes");
    assert_eq!(
        answer.as_ref().err().map(rift_session::Error::code),
        Some(CHANGED),
        "{answer:?}"
    );
}

#[test]
fn held_bytes_are_not_served_after_a_rewrite() {
    rewrite_after_a_hunk("LINE 15");
    rewrite_after_a_hunk("a longer line 15");
}
