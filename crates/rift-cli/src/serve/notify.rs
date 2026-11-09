//! Notifications: the progress of each scan and refinement, the end of
//! each scan, and refined segments the client looks at.
//!
//! One thread looks at every live session once per [`TICK`] and sends what
//! changed since its last look, so a session gets at most one batch of
//! notifications per tick, 20 a second, however fast its work advances.

use std::collections::VecDeque;
use std::time::Duration;

use rift_core::{DiffView, HunkState, Progress};
use rift_session::{MAX_PAGE, Session, SessionId, SessionState};
use serde_json::{Value, json};

use super::Server;
use super::methods::refined_status;
use super::wire::{note, session_error};

/// How often sessions are looked at.
pub const TICK: Duration = Duration::from_millis(50);

/// What has been sent about each live session.
#[derive(Debug, Default)]
pub struct Notes {
    sessions: Vec<Followed>,
}

#[derive(Debug)]
struct Followed {
    id: SessionId,
    scan: Option<u64>,
    refine: Option<Progress>,
    /// `session_state` has been sent.
    ended: bool,
    /// Pending segments in the last `set_visible` range, at most
    /// [`MAX_PAGE`].
    visible: Vec<usize>,
    /// Pending segments asked for with `hunk`, the latest [`MAX_PAGE`].
    asked: VecDeque<usize>,
}

impl Notes {
    fn get(&mut self, id: SessionId) -> &mut Followed {
        if let Some(i) = self.sessions.iter().position(|f| f.id == id) {
            return &mut self.sessions[i];
        }
        self.sessions.push(Followed {
            id,
            scan: None,
            refine: None,
            ended: false,
            visible: Vec::new(),
            asked: VecDeque::new(),
        });
        self.sessions.last_mut().expect("just pushed")
    }
}

impl Server {
    fn notes(&self) -> std::sync::MutexGuard<'_, Notes> {
        self.notes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reports segment `index` of session `id` when its refinement ends.
    pub fn watch_segment(&self, id: SessionId, index: usize) {
        let mut notes = self.notes();
        let f = notes.get(id);
        // The two lists stay disjoint, so a segment is reported once.
        if !f.asked.contains(&index) && !f.visible.contains(&index) {
            if f.asked.len() == MAX_PAGE {
                f.asked.pop_front();
            }
            f.asked.push_back(index);
        }
    }

    /// Reports `segments` of session `id` when their refinement ends,
    /// instead of those of the previous `set_visible`.
    pub fn show_segments(&self, id: SessionId, segments: Vec<usize>) {
        let mut notes = self.notes();
        let f = notes.get(id);
        f.asked.retain(|i| !segments.contains(i));
        f.visible = segments;
    }

    /// Sends what changed in every live session since the last call.
    pub fn notify(&self) {
        #[cfg(test)]
        tests::before_notes();
        // Everything below runs under the notes lock, which `close` answers
        // under: `observe` finds no session that was closed before this
        // point, so no note follows the answer to a close.
        let mut notes = self.notes();
        let ids = self.sessions.ids();
        notes.sessions.retain(|f| ids.contains(&f.id));
        let (mut out, mut ready) = (Vec::new(), Vec::new());
        for id in ids {
            // Held only while it is read, never across a send: a send waits
            // while the writer is behind, and a closed session held
            // meanwhile would keep its memory.
            if let Some(s) = self.sessions.observe(id) {
                look(notes.get(id), &s, &mut out, &mut ready);
            }
            for n in out.drain(..) {
                self.send(Box::new(n));
            }
            for (i, status) in ready.drain(..) {
                self.send(Box::new(note(
                    "segment_ready",
                    json!({"session": id.0, "index": i, "status": status}),
                )));
            }
        }
    }
}

/// Reads what changed in `s` since `f`: the notes to send into `out`, and
/// the refined segments, with their status, into `ready`.
fn look(
    f: &mut Followed,
    s: &Session,
    out: &mut Vec<Value>,
    ready: &mut Vec<(usize, &'static str)>,
) {
    let id = s.id().0;
    if !f.ended {
        // The state first: a scan that is ready has its final progress, so
        // reading them the other way round can report a ready session with
        // a progress short of its total, and never the total.
        let state = s.state();
        #[cfg(test)]
        tests::BETWEEN_READS.with(|h| h.get().map_or((), |hook| hook(s)));
        let (done, total) = s.scan_progress();
        if f.scan != Some(done.0) {
            f.scan = Some(done.0);
            out.push(note(
                "progress",
                json!({"session": id, "phase": "scan", "done": done.0, "total": total.0}),
            ));
        }
        match state {
            SessionState::Scanning => return,
            SessionState::Ready => {
                out.push(note(
                    "session_state",
                    json!({"session": id, "state": "ready"}),
                ));
            }
            SessionState::Failed => {
                let error = s.error().map(|e| session_error(e, None).object());
                out.push(note(
                    "session_state",
                    json!({"session": id, "state": "failed", "error": error}),
                ));
            }
        }
        f.ended = true;
    }
    let Ok(diff) = s.diff() else {
        return;
    };
    let p = diff.progress();
    if f.refine != Some(p) {
        f.refine = Some(p);
        out.push(note(
            "progress",
            json!({"session": id, "phase": "refine", "done": p.done, "total": p.total}),
        ));
    }
    let segs = diff.segments();
    let mut refined = |i: usize| match diff.try_hunk(i) {
        Ok(HunkState::Pending) => true,
        Ok(state) => {
            if let Some(status) = refined_status(&state, &segs[i]) {
                ready.push((i, status));
            }
            false
        }
        // A failed refinement is reported by `hunk`.
        Err(_) => false,
    };
    f.visible.retain(|&i| refined(i));
    f.asked.retain(|&i| refined(i));
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::sync::mpsc::sync_channel;
    use std::sync::{Arc, Mutex};

    use rift_session::{Limits, Session, Sessions};
    use serde_json::json;

    use super::super::Server;

    #[test]
    fn a_tick_waiting_for_the_writer_leaves_its_sessions_free() {
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str, text: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, text).unwrap();
            p.to_str().unwrap().to_owned()
        };
        let (a, b, c) = (path("a", "one\n"), path("b", "two\n"), path("c", "three\n"));
        // No room in the queue: every send waits for this test to take it.
        let (tx, rx) = sync_channel(0);
        let server = Arc::new(Server {
            sessions: Sessions::new(Limits {
                sessions: 1,
                ..Limits::default()
            }),
            max_session_mem: Limits::default().session_mem,
            default_mem: rift_session::DEFAULT_MEM,
            out: tx,
            flights: Mutex::default(),
            room: std::sync::Condvar::new(),
            notes: Mutex::default(),
        });
        let session = server.sessions.open(&a, &b, None).unwrap().session;
        session.wait();
        drop(session);
        let ticker = {
            let server = Arc::clone(&server);
            std::thread::spawn(move || server.notify())
        };
        // A ready session's first tick has more than one note: the ticker
        // is now past its first send and waits on the next one.
        rx.recv().unwrap();
        let opened = server.sessions.open(&a, &c, None);
        drop(rx);
        ticker.join().unwrap();
        assert!(opened.is_ok(), "{opened:?}");
    }

    thread_local! {
        /// Runs in `look` between the read of the state and that of the
        /// progress, on the thread that looks.
        pub(super) static BETWEEN_READS: Cell<Option<fn(&Session)>> = const { Cell::new(None) };
    }

    /// A look that reports the end of a scan has reported its progress as
    /// complete. The scan ends between the two reads, which the hook makes
    /// happen on every look: read in the other order, the first look sends
    /// the progress of before the end and then `ready`.
    #[test]
    fn the_progress_sent_with_the_end_of_a_scan_is_complete() {
        use std::fmt::Write;

        use super::{Notes, look};
        fn wait_for_the_end(s: &Session) {
            s.wait();
        }
        BETWEEN_READS.with(|h| h.set(Some(wait_for_the_end)));
        let dir = tempfile::tempdir().unwrap();
        let mut text = String::new();
        for i in 0..20_000 {
            writeln!(text, "line {i}").unwrap();
        }
        let path = |name: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, &text).unwrap();
            p.to_str().unwrap().to_owned()
        };
        let (a, b) = (path("a"), path("b"));
        let sessions = Sessions::new(Limits::default());
        let session = sessions.open(&a, &b, None).unwrap().session;
        let (mut notes, mut last) = (Notes::default(), None);
        for _ in 0..3 {
            let (mut out, mut ready) = (Vec::new(), Vec::new());
            look(notes.get(session.id()), &session, &mut out, &mut ready);
            for n in &out {
                match n["method"].as_str().unwrap() {
                    "progress" if n["params"]["phase"] == "scan" => {
                        last = Some(n["params"].clone());
                    }
                    "session_state" => {
                        let p = last.as_ref().expect("a progress before the end");
                        assert_eq!(p["done"], p["total"], "ready after {p}");
                        BETWEEN_READS.with(|h| h.set(None));
                        return;
                    }
                    _ => {}
                }
            }
        }
        panic!("the scan never ended");
    }

    thread_local! {
        /// Runs at the start of `notify`, on the thread that ticks.
        static BEFORE_NOTES: RefCell<Option<Box<dyn Fn()>>> = const { RefCell::new(None) };
    }

    pub(super) fn before_notes() {
        BEFORE_NOTES.with(|h| {
            if let Some(hook) = &*h.borrow() {
                hook();
            }
        });
    }

    /// `close` answers under the notes lock, so a tick that lists the live
    /// sessions after that has nothing to say about the session. The close
    /// runs, through the real request path, in the gap between the start of
    /// a tick and the point it lists the sessions.
    #[test]
    fn no_note_follows_the_answer_to_a_close() {
        use std::sync::atomic::AtomicBool;
        use std::sync::mpsc::channel;

        use super::super::Flight;
        use super::super::methods::Call;
        use super::super::wire::Body;
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str, text: &str| {
            let p = dir.path().join(name);
            std::fs::write(&p, text).unwrap();
            p.to_str().unwrap().to_owned()
        };
        let (a, b) = (path("a", "one\n"), path("b", "two\n"));
        let (tx, rx) = sync_channel::<Box<dyn Body>>(64);
        let server = Arc::new(Server {
            sessions: Sessions::new(Limits::default()),
            max_session_mem: Limits::default().session_mem,
            default_mem: rift_session::DEFAULT_MEM,
            out: tx,
            flights: Mutex::default(),
            room: std::sync::Condvar::new(),
            notes: Mutex::default(),
        });
        // Held by this test, as by a request that is still running on it.
        let session = server.sessions.open(&a, &b, None).unwrap().session;
        session.wait();
        let (started, start) = channel();
        let (go, resume) = channel();
        let ticker = {
            let server = Arc::clone(&server);
            std::thread::spawn(move || {
                BEFORE_NOTES.with(|h| {
                    *h.borrow_mut() = Some(Box::new(move || {
                        started.send(()).unwrap();
                        resume.recv().unwrap();
                    }));
                });
                server.notify();
            })
        };
        start.recv().unwrap();
        let flight = Flight {
            id: json!(1),
            answered: AtomicBool::new(false),
        };
        server.run(&flight, Call::Close(session.id()));
        go.send(()).unwrap();
        ticker.join().unwrap();
        drop(server);
        let mut closed = false;
        let mut late = Vec::new();
        while let Ok(body) = rx.recv() {
            let mut text = Vec::new();
            body.write(&mut text).unwrap();
            let text = String::from_utf8(text).unwrap();
            if text.contains("\"closed\"") {
                closed = true;
            } else if closed {
                late.push(text);
            }
        }
        assert!(closed, "no answer to the close");
        assert!(late.is_empty(), "sent after the close answer: {late:?}");
        drop(session);
    }
}
