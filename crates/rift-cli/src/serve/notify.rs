//! Notifications: the progress of each scan and refinement, the end of
//! each scan, and refined segments the client looks at.
//!
//! One thread looks at every live session once per [`TICK`] and sends what
//! changed since its last look, so a session gets at most one batch of
//! notifications per tick, 20 a second, however fast its work advances.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use rift_core::{DiffView, HunkState, Progress};
use rift_session::{MAX_PAGE, Session, SessionId, SessionState};
use serde_json::{Value, json};

use super::Server;
use super::methods::refined_status;
use super::wire::{note, scan_progress, session_error};

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
        let live: Vec<_> = self
            .sessions
            .live()
            .iter()
            .map(|s| (s.id(), Arc::downgrade(s)))
            .collect();
        let mut notes = self.notes();
        notes
            .sessions
            .retain(|f| live.iter().any(|(id, _)| *id == f.id));
        let (mut out, mut ready) = (Vec::new(), Vec::new());
        for (id, session) in &live {
            // Held only while it is read, never across a send: a send waits
            // while the writer is behind, and a session held meanwhile looks
            // in use, so an `open` that needs its room would get `busy`.
            if let Some(s) = session.upgrade() {
                look(notes.get(*id), &s, &mut out, &mut ready);
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
        let (done, _) = s.scan_progress();
        if f.scan != Some(done.0) {
            f.scan = Some(done.0);
            let mut params = scan_progress(s);
            params["session"] = json!(id);
            out.push(note("progress", params));
        }
        match s.state() {
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
    use std::sync::mpsc::sync_channel;
    use std::sync::{Arc, Mutex};

    use rift_session::{Limits, Sessions};

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
}
