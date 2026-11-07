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
//!   call fails with [`Error::Busy`].

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rift_core::diff::Diff;
use rift_core::{Budget, Bytes, Half, Reservation, Source};

use crate::error::{Error, Side};
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
/// The response pool of a session opened by path is this fraction,
/// `1 / RESPONSE_SHARE`, of its Refine half, and at most
/// [`MAX_RESPONSE_POOL`].
pub const RESPONSE_SHARE: u64 = 16;
/// The largest response pool.
pub const MAX_RESPONSE_POOL: Bytes = Bytes::mib(8);

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

/// The id of a session, unique for the life of its [`Sessions`]. An id is
/// never given out twice, so a stale one is reported, never confused with
/// a newer session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub u64);

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Where a session is in its life, as the protocol reports it.
///
/// A session from [`Sessions::open`] or [`Sessions::get`] is built before
/// the call returns, so it is always `Ready`. `rift serve` answers `open`
/// before the scan ends and keeps the other two states for its sessions
/// until then, as `docs/protocol.md` describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    /// The scan is building the segment map.
    Scanning,
    /// The map is built; every method answers.
    Ready,
    /// The scan failed; the session holds no budget.
    Failed,
}

impl SessionState {
    /// The protocol's name: `scanning`, `ready` or `failed`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Scanning => "scanning",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

type Key = (FileId, FileId);

/// Memory the responses of a session are built in: a share of the
/// session's Refine half, held for the life of the session and handed
/// out through a budget of its own.
#[derive(Debug)]
pub(crate) struct Pool {
    pub(crate) budget: Budget,
    _held: Reservation,
}

/// The diff of one pair and the memory it was given.
#[derive(Debug)]
pub struct Session {
    pub(crate) id: SessionId,
    pub(crate) diff: Diff<FileSource, FileSource>,
    pub(crate) budget: Budget,
    pub(crate) mem: Bytes,
    pub(crate) bytes: [Bytes; 2],
    pub(crate) pool: Option<Pool>,
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
    last_id: u64,
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

    /// The limits the set was made with.
    #[must_use]
    pub fn limits(&self) -> &Limits {
        &self.limits
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
    /// A session built here has no response pool: an
    /// [`excerpt`](Session::excerpt) of it is paid from whatever its Refine
    /// half has free. `rift-mcp` renders its answers itself and uses this.
    ///
    /// # Errors
    ///
    /// [`Error::MemOverLimit`], [`Error::Busy`], or [`Error::Build`] with
    /// the core's error while scanning.
    pub fn get(
        &self,
        a: (FileSource, FileId),
        b: (FileSource, FileId),
        mem: Option<Bytes>,
    ) -> Result<Arc<Session>, Error> {
        self.find_or_build(a, b, mem, false)
    }

    /// Opens the files at `a` and `b` and returns their session, built
    /// with `mem` (the default when `None`) if none is live, with a
    /// response pool for [`excerpt`](Session::excerpt).
    ///
    /// Blocks for the scan: two passes over both files.
    ///
    /// # Errors
    ///
    /// [`Error::File`] naming the side, then as for [`get`](Self::get).
    pub fn open(&self, a: &str, b: &str, mem: Option<Bytes>) -> Result<Arc<Session>, Error> {
        let file = |side, path| FileSource::open(path).map_err(|error| Error::File { side, error });
        let a = file(Side::A, a)?;
        let b = file(Side::B, b)?;
        self.find_or_build(a, b, mem, true)
    }

    /// The live session `id`, marked as used now.
    ///
    /// # Errors
    ///
    /// [`Error::UnknownSession`] when it was closed or dropped.
    pub fn session(&self, id: SessionId) -> Result<Arc<Session>, Error> {
        let mut s = self.lock();
        let e = s
            .live
            .iter_mut()
            .find(|e| e.session.id == id)
            .ok_or(Error::UnknownSession(id))?;
        e.used = Instant::now();
        Ok(Arc::clone(&e.session))
    }

    /// Drops session `id` from the set. Calls holding it finish first; its
    /// memory is released when the last of them does. Returns whether it
    /// was live.
    pub fn close(&self, id: SessionId) -> bool {
        let mut s = self.lock();
        let before = s.live.len();
        s.live.retain(|e| e.session.id != id);
        s.live.len() < before
    }

    fn find_or_build(
        &self,
        (a, id_a): (FileSource, FileId),
        (b, id_b): (FileSource, FileId),
        mem: Option<Bytes>,
        pool: bool,
    ) -> Result<Arc<Session>, Error> {
        let key = (id_a, id_b);
        let mem = mem.unwrap_or(DEFAULT_MEM.min(self.limits.session_mem));
        if mem > self.limits.session_mem {
            return Err(Error::MemOverLimit {
                mem,
                limit: self.limits.session_mem,
            });
        }
        let (_building, id) = {
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
                    return Err(Error::Busy);
                };
                s.live.swap_remove(i);
            }
            s.building += 1;
            s.building_mem += mem.0;
            s.last_id += 1;
            let building = Building {
                sessions: self,
                mem,
            };
            (building, SessionId(s.last_id))
        };
        let bytes = [a.len(), b.len()];
        let budget = Budget::new(mem);
        let build = |error| Error::Build { mem, error };
        // Taken before the diff, which sizes its workers and cache from
        // what the Refine half has left.
        let pool = if pool {
            let share =
                Bytes((budget.capacity(Half::Refine).0 / RESPONSE_SHARE).min(MAX_RESPONSE_POOL.0));
            let held = budget
                .try_reserve(Half::Refine, share)
                .map_err(|e| build(e.into()))?;
            Some(Pool {
                budget: Budget::new(Bytes(2 * share.0)),
                _held: held,
            })
        } else {
            None
        };
        let threads = std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(4);
        let diff = Diff::new(Arc::new(a), Arc::new(b), &budget, threads).map_err(build)?;
        let session = Arc::new(Session {
            id,
            diff,
            budget,
            mem,
            bytes,
            pool,
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

#[cfg(fuzzing)]
#[doc(hidden)]
impl Session {
    pub fn fuzz_diff(&self) -> &Diff<FileSource, FileSource> {
        &self.diff
    }
}
