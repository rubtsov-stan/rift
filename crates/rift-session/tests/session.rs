//! Sessions opened by path, as `rift serve` uses them, on small files.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rift_core::{BudgetExceeded, Bytes, DiffView, HunkState, LineNo, SegmentStatus};
use rift_session::file::Reason;
use rift_session::{
    At, Eol, Error, ExcerptRequest, Limits, MovesState, Session, Sessions, Side, code,
};

fn write(dir: &Path, name: &str, bytes: &[u8]) -> String {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path.to_str().unwrap().to_owned()
}

fn numbered(range: std::ops::RangeInclusive<u32>) -> Vec<u8> {
    range
        .flat_map(|i| format!("line {i}\n").into_bytes())
        .collect()
}

/// A has lines 1 to 40; B changes line 10.
fn pair(dir: &Path) -> (String, String) {
    let a = numbered(1..=40);
    let b = String::from_utf8(a.clone())
        .unwrap()
        .replace("line 10\n", "changed 10\n");
    (write(dir, "a", &a), write(dir, "b", b.as_bytes()))
}

fn sessions() -> Sessions {
    Sessions::new(Limits {
        idle: Duration::from_secs(3600),
        ..Limits::default()
    })
}

const MEM: Option<Bytes> = Some(Bytes::mib(16));

fn open(s: &Sessions, a: &str, b: &str) -> Arc<Session> {
    s.open(a, b, MEM).unwrap()
}

fn texts(lines: &[rift_session::Line]) -> Vec<&str> {
    lines.iter().map(|l| l.text.as_str()).collect()
}

/// Waits for the refined state of segment `i`.
fn refined(s: &Session, i: usize) -> HunkState {
    let began = Instant::now();
    loop {
        match s.hunk_state(i).unwrap() {
            HunkState::Pending if began.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(1));
            }
            state => return state,
        }
    }
}

#[test]
fn a_pair_opened_twice_is_one_session() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let s = sessions();
    let first = open(&s, &a, &b);
    let again = s.open(&a, &b, Some(Bytes::mib(32))).unwrap();
    assert_eq!(first.id(), again.id());
    assert_eq!(
        again.mem(),
        Bytes::mib(16),
        "a live session keeps its budget"
    );
    assert_eq!(s.len(), 1);
    assert_eq!(s.session(first.id()).unwrap().id(), first.id());
}

#[test]
fn a_file_error_names_the_side_and_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let (a, _) = pair(dir.path());
    let err = sessions().open(&a, "/nonexistent/rift", MEM).unwrap_err();
    assert_eq!(err.code(), code::FILE);
    let Error::File { side, error } = &err else {
        panic!("{err:?}")
    };
    assert_eq!(*side, Side::B);
    assert_eq!(error.path, "/nonexistent/rift");
    assert!(matches!(error.reason, Reason::Io(_)));
    assert_eq!(error.reason.name(), "not_found");
    assert!(
        err.to_string().starts_with("b: /nonexistent/rift: "),
        "{err}"
    );
}

#[test]
fn a_budget_over_the_session_limit_is_too_large() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let err = sessions().open(&a, &b, Some(Bytes::mib(257))).unwrap_err();
    assert_eq!(err.code(), code::TOO_LARGE);
    assert_eq!(
        err.to_string(),
        "mem 269484032 is over the limit of 268435456 per diff"
    );
}

#[test]
fn a_budget_refusal_suggests_the_mem_the_cli_would() {
    let dir = tempfile::tempdir().unwrap();
    let a = numbered(1..=20_000);
    let mut b = a.clone();
    b.extend_from_slice(b"one more\n");
    let (a, b) = (write(dir.path(), "a", &a), write(dir.path(), "b", &b));
    let mem = Bytes(64 << 10);
    let err = sessions().open(&a, &b, Some(mem)).unwrap_err();
    assert_eq!(err.code(), code::BUDGET_EXCEEDED, "{err}");
    let (held, refusal) = err.budget().unwrap();
    assert_eq!(held, mem);
    let suggested = err.suggested_mem().unwrap();
    assert_eq!(Some(suggested), refusal.mem_advice(mem));
    assert!(suggested > mem);
}

#[test]
fn a_closed_session_is_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let s = sessions();
    let id = open(&s, &a, &b).id();
    assert!(s.close(id));
    assert!(!s.close(id), "closing twice is not an error");
    let err = s.session(id).unwrap_err();
    assert_eq!(err.code(), code::UNKNOWN_SESSION);
    assert!(s.is_empty());
}

#[test]
fn sessions_in_use_make_a_new_pair_wait() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let c = write(dir.path(), "c", b"other\n");
    let s = Sessions::new(Limits {
        sessions: 1,
        ..Limits::default()
    });
    let held = open(&s, &a, &b);
    let err = s.open(&a, &c, MEM).unwrap_err();
    assert_eq!(err.code(), code::BUSY);
    drop(held);
    open(&s, &a, &c);
}

#[test]
fn the_summary_counts_segments_and_moves() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let s = sessions();
    let sum = open(&s, &a, &b).summary();
    assert_eq!((sum.a_lines, sum.b_lines), (40, 40));
    assert_eq!(sum.a_bytes, Bytes(std::fs::metadata(&a).unwrap().len()));
    assert!(!sum.identical);
    assert_eq!(sum.differing, 1);
    assert_eq!(sum.differs.segments + sum.approx.segments, 1);
    assert_eq!(sum.moves, MovesState::Found);
    assert_eq!(sum.mem, Bytes::mib(16));
    assert!(sum.reserved <= sum.mem);
}

#[test]
fn an_excerpt_shows_both_sides_of_a_change_with_context() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let s = sessions();
    let session = open(&s, &a, &b);
    let mut req = ExcerptRequest::new(At::LineA(LineNo(9)));
    req.context = 2;
    let e = session.excerpt(&req).unwrap();
    let seg = session.diff().segments()[e.segment].clone();
    assert!(seg.a.contains(&LineNo(9)), "{seg:?}");
    assert_eq!(e.range, seg);
    assert_eq!(e.a.start, LineNo(seg.a.start.0 - 2));
    assert_eq!(e.a.total, seg.len_a() + 4);
    assert_eq!(e.a.lines.len() as u64, e.a.total);
    assert!(texts(&e.a.lines).contains(&"line 10"));
    assert!(texts(&e.b.lines).contains(&"changed 10"));
    assert!(e.a.lines.iter().all(|l| l.eol == Eol::Lf && !l.lossy));
    assert!(!e.truncated());
    let by_index = session
        .excerpt(&ExcerptRequest {
            at: At::Segment(e.segment),
            ..req
        })
        .unwrap();
    assert_eq!(texts(&by_index.b.lines), texts(&e.b.lines));
}

#[test]
fn an_excerpt_reports_refinement() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let session = open(&sessions(), &a, &b);
    let e = session
        .excerpt(&ExcerptRequest::new(At::LineA(LineNo(9))))
        .unwrap();
    assert!(matches!(refined(&session, e.segment), HunkState::Ready(_)));
    let e = session
        .excerpt(&ExcerptRequest::new(At::Segment(e.segment)))
        .unwrap();
    assert!(!e.pending);
    assert_eq!(e.status, SegmentStatus::Differs);
}

#[test]
fn an_equal_segment_reached_by_line_starts_at_that_line() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let session = open(&sessions(), &a, &b);
    let mut req = ExcerptRequest::new(At::LineA(LineNo(30)));
    req.context = 1;
    req.max_lines = 3;
    let e = session.excerpt(&req).unwrap();
    assert_eq!(e.status, SegmentStatus::Equal);
    assert!(!e.pending);
    assert_eq!(texts(&e.a.lines), ["line 30", "line 31", "line 32"]);
    assert_eq!(texts(&e.b.lines), texts(&e.a.lines));
    assert!(e.truncated());
}

#[test]
fn line_text_carries_its_terminator_and_its_losses() {
    let dir = tempfile::tempdir().unwrap();
    let long = "é".repeat(3000);
    let a = write(dir.path(), "a", b"first\n");
    let mut b = b"caf\xff\r\n".to_vec();
    b.extend(format!("{long}\nlast").bytes());
    let b = write(dir.path(), "b", &b);
    let session = open(&sessions(), &a, &b);
    let mut req = ExcerptRequest::new(At::Segment(0));
    req.max_line_bytes = 101;
    let e = session.excerpt(&req).unwrap();
    let lines = &e.b.lines;
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].text, "caf\u{fffd}");
    assert!(lines[0].lossy && !lines[0].truncated);
    assert_eq!((lines[0].bytes, lines[0].eol), (4, Eol::CrLf));
    // 101 bytes end inside the 51st two-byte character, which is dropped.
    assert_eq!(lines[1].text, "é".repeat(50));
    assert!(lines[1].truncated && !lines[1].lossy);
    assert_eq!((lines[1].bytes, lines[1].eol), (6000, Eol::Lf));
    assert_eq!((lines[2].text.as_str(), lines[2].eol), ("last", Eol::None));
}

#[test]
fn max_lines_cuts_each_side_and_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let a = write(dir.path(), "a", &numbered(1..=50));
    let b = write(dir.path(), "b", b"other\n");
    let session = open(&sessions(), &a, &b);
    let mut req = ExcerptRequest::new(At::Segment(0));
    req.max_lines = 7;
    let e = session.excerpt(&req).unwrap();
    assert_eq!(e.a.lines.len(), 7);
    assert_eq!(e.a.total, 50);
    assert_eq!(e.b.lines.len(), 1);
    assert!(e.truncated());
}

#[test]
fn moved_lines_are_flagged_on_both_sides() {
    let dir = tempfile::tempdir().unwrap();
    let block: Vec<u8> = (0..5)
        .flat_map(|i| format!("moved row {i}\n").into_bytes())
        .collect();
    let mut a = block.clone();
    a.extend(numbered(1..=60));
    let mut b = numbered(1..=60);
    b.extend(&block);
    let (a, b) = (write(dir.path(), "a", &a), write(dir.path(), "b", &b));
    let session = open(&sessions(), &a, &b);
    let blocks = session.diff().moved_blocks();
    assert_eq!(blocks.len(), 1, "{blocks:?}");
    let e = session
        .excerpt(&ExcerptRequest::new(At::LineA(LineNo(0))))
        .unwrap();
    let moved = |lines: &[rift_session::Line]| {
        lines
            .iter()
            .filter(|l| l.moved)
            .map(|l| l.text.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(moved(&e.a.lines).len(), 5);
    let last = session.diff().segments().len() - 1;
    let e = session
        .excerpt(&ExcerptRequest::new(At::Segment(last)))
        .unwrap();
    assert_eq!(moved(&e.b.lines).len(), 5);
    assert!(moved(&e.b.lines)[0].starts_with("moved row"));
}

#[test]
fn positions_past_the_end_are_invalid_params() {
    let dir = tempfile::tempdir().unwrap();
    let (a, b) = pair(dir.path());
    let session = open(&sessions(), &a, &b);
    let n = session.diff().segments().len();
    let err = session
        .excerpt(&ExcerptRequest::new(At::Segment(n)))
        .unwrap_err();
    assert_eq!(err.code(), code::INVALID_PARAMS);
    let err = session
        .excerpt(&ExcerptRequest::new(At::LineA(LineNo(40))))
        .unwrap_err();
    assert_eq!(err.to_string(), "line 41 is out of range, a has 40 lines");
    assert_eq!(
        session.hunk_state(n).unwrap_err().code(),
        code::INVALID_PARAMS
    );
}

#[test]
fn excerpts_share_the_response_pool_and_return_it() {
    let dir = tempfile::tempdir().unwrap();
    // 2000 lines of 1000 bytes on each side, every one different: more
    // than the pool of a 32 MiB session (1/16 of 16 MiB) holds.
    let side = |tag: &str| -> Vec<u8> {
        (0..2000)
            .flat_map(|i| format!("{tag} {i:04} {}\n", "x".repeat(990)).into_bytes())
            .collect()
    };
    let a = write(dir.path(), "a", &side("old"));
    let b = write(dir.path(), "b", &side("new"));
    let session = Sessions::new(Limits::default())
        .open(&a, &b, Some(Bytes::mib(32)))
        .unwrap();
    let mut req = ExcerptRequest::new(At::LineA(LineNo(0)));
    req.max_lines = 5000;
    let first = session.excerpt(&req).unwrap();
    assert!(first.truncated(), "the pool cuts the excerpt");
    let shown: usize = first
        .a
        .lines
        .iter()
        .chain(&first.b.lines)
        .map(|l| l.text.len())
        .sum();
    assert!(shown as u64 <= Bytes::mib(32).0 / 2 / 16, "{shown}");
    assert!(shown > 900 * 1000, "{shown}");
    let err = session.excerpt(&req).unwrap_err();
    assert_eq!(err.code(), code::BUSY);
    assert!(matches!(err, Error::ResponsesBusy));
    drop(first);
    session.excerpt(&req).unwrap();
}

#[test]
fn core_errors_map_to_their_codes() {
    let mem = Bytes::mib(1);
    let build = |error| Error::Build { mem, error };
    let refusal = BudgetExceeded {
        half: rift_core::Half::Skeleton,
        requested: Bytes(1 << 20),
        available: Bytes(0),
    };
    assert_eq!(build(refusal.into()).code(), code::BUDGET_EXCEEDED);
    let io = std::io::Error::other("gone");
    assert_eq!(build(io.into()).code(), code::IO);
    let script = rift_core::Error::InvalidScript("x".into());
    let err = Error::Segment {
        segment: 3,
        mem,
        error: script,
    };
    assert_eq!(err.code(), code::INTERNAL_ERROR);
    assert_eq!(err.suggested_mem(), None);
    let err = build(refusal.into());
    assert_eq!(err.suggested_mem(), Some(Bytes(3 << 20)));
}

#[test]
fn a_session_that_is_not_ready_has_its_own_code() {
    let err = Error::NotReady {
        session: rift_session::SessionId(4),
        state: rift_session::SessionState::Scanning,
    };
    assert_eq!(err.code(), code::NOT_READY);
    assert_eq!(
        err.to_string(),
        "session 4 is scanning; wait for it to be ready"
    );
    assert_eq!(rift_session::SessionState::Failed.name(), "failed");
}
