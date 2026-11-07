//! Notifications: the progress of each scan and refinement, the end of
//! each scan, and refined segments the client looks at.
//!
//! One thread looks at every live session once per [`TICK`] and sends what
//! changed since its last look, so a session gets at most one batch of
//! notifications per tick, 20 a second, however fast its work advances.

use std::collections::VecDeque;
use std::time::Duration;

use rift_core::{DiffView, HunkState, Progress};
use rift_session::{MAX_PAGE, SessionId, SessionState};
use serde_json::json;

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
        let live = self.sessions.live();
        let mut notes = self.notes();
        notes
            .sessions
            .retain(|f| live.iter().any(|s| s.id() == f.id));
        for s in &live {
            let id = s.id().0;
            let f = notes.get(s.id());
            let state = s.state();
            if !f.ended {
                let (done, _) = s.scan_progress();
                if f.scan != Some(done.0) {
                    f.scan = Some(done.0);
                    let mut params = scan_progress(s);
                    params["session"] = json!(id);
                    self.send(Box::new(note("progress", params)));
                }
                match state {
                    SessionState::Scanning => continue,
                    SessionState::Ready => {
                        self.send(Box::new(note(
                            "session_state",
                            json!({"session": id, "state": "ready"}),
                        )));
                    }
                    SessionState::Failed => {
                        let error = s.error().map(|e| session_error(e, None).object());
                        self.send(Box::new(note(
                            "session_state",
                            json!({"session": id, "state": "failed", "error": error}),
                        )));
                    }
                }
                f.ended = true;
            }
            let Ok(diff) = s.diff() else {
                continue;
            };
            let p = diff.progress();
            if f.refine != Some(p) {
                f.refine = Some(p);
                self.send(Box::new(note(
                    "progress",
                    json!({"session": id, "phase": "refine", "done": p.done, "total": p.total}),
                )));
            }
            let segs = diff.segments();
            let refined = |i: usize| match diff.try_hunk(i) {
                Ok(HunkState::Pending) => true,
                Ok(state) => {
                    if let Some(status) = refined_status(&state, &segs[i]) {
                        self.send(Box::new(note(
                            "segment_ready",
                            json!({"session": id, "index": i, "status": status}),
                        )));
                    }
                    false
                }
                // A failed refinement is reported by `hunk`.
                Err(_) => false,
            };
            f.visible.retain(|&i| refined(i));
            f.asked.retain(|&i| refined(i));
        }
    }
}
