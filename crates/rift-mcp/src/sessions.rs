//! One live diff per pair of files, with a bounded count and memory.
//!
//! Policy:
//! - a session holds the skeleton and the refinement workers of one
//!   `(a, b)` pair, so later calls on the pair skip the scan;
//! - at most [`Limits::sessions`] sessions exist, and the `--mem` budgets of
//!   all of them (and of the ones being built) sum to at most
//!   [`Limits::total_mem`], so the server's accounted heap stays below that
//!   figure plus one bounded response;
//! - a new session first evicts idle ones, then the least recently used
//!   ones, but never a session a call is using; when that is not enough the
//!   call fails with a message saying the server is busy.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rift_core::diff::Diff;
use rift_core::{Budget, Bytes, Half, Source};

use crate::file::{FileId, FileSource};

/// Memory a session gets when the call does not say.
pub const DEFAULT_MEM: Bytes = Bytes::mib(64);
/// Most memory one session may ask for.
pub const MAX_SESSION_MEM: Bytes = Bytes::mib(256);
/// Sum of the budgets of all live and building sessions.
pub const MAX_TOTAL_MEM: Bytes = Bytes::mib(512);
/// Live sessions.
pub const MAX_SESSIONS: usize = 4;
/// A session unused for this long is dropped.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// The knobs of the policy above; tests shrink them.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Live sessions.
    pub sessions: usize,
    /// Memory budget of one session at most.
    pub session_mem: Bytes,
    /// Memory budgets of all sessions together at most.
    pub total_mem: Bytes,
    /// Idle time after which a session is dropped.
    pub idle: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            sessions: MAX_SESSIONS,
            session_mem: MAX_SESSION_MEM,
            total_mem: MAX_TOTAL_MEM,
            idle: IDLE_TIMEOUT,
        }
    }
}

type Key = (FileId, FileId);

/// The diff of one pair and the memory it was given.
#[derive(Debug)]
pub struct Session {
    pub(crate) diff: Diff<FileSource, FileSource>,
    pub(crate) budget: Budget,
    pub(crate) mem: Bytes,
    pub(crate) bytes: [Bytes; 2],
}

#[derive(Debug)]
struct Entry {
    key: Key,
    session: Arc<Session>,
    used: Instant,
}

#[derive(Debug, Default)]
struct State {
    live: Vec<Entry>,
    /// Sessions being built, which hold budget but are not in `live` yet.
    building: usize,
    building_mem: u64,
}

impl State {
    fn mem(&self) -> u64 {
        self.building_mem + self.live.iter().map(|e| e.session.mem.0).sum::<u64>()
    }

    fn fits(&self, mem: Bytes, limits: &Limits) -> bool {
        self.live.len() + self.building < limits.sessions
            && self.mem().saturating_add(mem.0) <= limits.total_mem.0
    }
}

/// The sessions of a server.
#[derive(Debug)]
pub struct Sessions {
    state: Mutex<State>,
    limits: Limits,
}

/// Held while a session is being built; returns its share on drop.
struct Building<'s> {
    sessions: &'s Sessions,
    mem: Bytes,
}

impl Drop for Building<'_> {
    fn drop(&mut self) {
        let mut s = self.sessions.lock();
        s.building -= 1;
        s.building_mem -= self.mem.0;
    }
}

impl Sessions {
    /// An empty set under `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            state: Mutex::default(),
            limits,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // The state stays consistent across a panic: every update is one
        // statement.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Live sessions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.lock().live.len()
    }

    /// Whether no session is live.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Sum of the memory budgets of the live sessions.
    #[must_use]
    pub fn mem(&self) -> Bytes {
        Bytes(self.lock().mem())
    }

    /// Drops sessions that have been idle for the idle timeout and are not in
    /// use. Returns how many.
    pub fn evict_idle(&self) -> usize {
        let mut s = self.lock();
        let before = s.live.len();
        let idle = self.limits.idle;
        s.live
            .retain(|e| Arc::strong_count(&e.session) > 1 || e.used.elapsed() < idle);
        before - s.live.len()
    }

    /// The session for `(a, b)`, built with `mem` (the default when `None`)
    /// if none is live. `mem` is ignored for an existing session.
    ///
    /// # Errors
    ///
    /// A message for the client: `mem` over the limits, the server busy, or
    /// the core's error while scanning.
    pub fn get(
        &self,
        (a, id_a): (FileSource, FileId),
        (b, id_b): (FileSource, FileId),
        mem: Option<Bytes>,
    ) -> Result<Arc<Session>, String> {
        let key = (id_a, id_b);
        let mem = mem.unwrap_or(DEFAULT_MEM.min(self.limits.session_mem));
        if mem > self.limits.session_mem {
            return Err(format!(
                "mem {mem} is over the limit of {} per diff",
                self.limits.session_mem
            ));
        }
        let _building = {
            let mut s = self.lock();
            if let Some(e) = s.live.iter_mut().find(|e| e.key == key) {
                e.used = Instant::now();
                return Ok(Arc::clone(&e.session));
            }
            let idle = self.limits.idle;
            s.live
                .retain(|e| Arc::strong_count(&e.session) > 1 || e.used.elapsed() < idle);
            while !s.fits(mem, &self.limits) {
                let lru = s
                    .live
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| Arc::strong_count(&e.session) == 1)
                    .min_by_key(|(_, e)| e.used)
                    .map(|(i, _)| i);
                let Some(i) = lru else {
                    return Err("the server is busy with other diffs; retry later".into());
                };
                s.live.swap_remove(i);
            }
            s.building += 1;
            s.building_mem += mem.0;
            Building {
                sessions: self,
                mem,
            }
        };
        let bytes = [a.len(), b.len()];
        let budget = Budget::new(mem);
        let threads = std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(4);
        let diff = Diff::new(Arc::new(a), Arc::new(b), &budget, threads)
            .map_err(|e| format!("cannot diff within {mem}: {e}"))?;
        let session = Arc::new(Session {
            diff,
            budget,
            mem,
            bytes,
        });
        // Two first calls on the same new pair both scan it and the later
        // insert wins. That costs one extra scan and its budget while both
        // run. A per-key build latch would fix it if concurrent first calls
        // on one pair turn out to matter.
        let mut s = self.lock();
        s.live.retain(|e| e.key != key);
        s.live.push(Entry {
            key,
            session: Arc::clone(&session),
            used: Instant::now(),
        });
        drop(s);
        Ok(session)
    }
}

impl Session {
    /// Bytes reserved from both halves of the session's budget.
    pub(crate) fn reserved(&self) -> Bytes {
        Bytes(self.budget.used(Half::Skeleton).0 + self.budget.used(Half::Refine).0)
    }
}
