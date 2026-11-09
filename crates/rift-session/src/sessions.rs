//! One live diff per pair of files, with a bounded count and memory.
//!
//! Policy:
//! - a session holds the skeleton and the refinement workers of one
//!   `(a, b)` pair, so later calls on the pair skip the scan;
//! - at most [`Limits::sessions`] sessions are live, and the `--mem`
//!   budgets of every session that still holds memory (live, being built,
//!   or closed while its scan winds down) sum to at most
//!   [`Limits::total_mem`], so the server's accounted heap stays below that
//!   figure plus its responses; a failed session holds no budget;
//! - a new session first evicts idle ones, then, while it does not fit,
//!   failed ones, then the least recently used ready ones that no call is
//!   using; a scanning session is never evicted. When that is not enough
//!   the call fails with [`Error::Busy`].

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use rift_core::diff::{Config, Diff};
use rift_core::scan::Watch;
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
/// The response pool of a session is this fraction,
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
/// A session from [`Sessions::open`] starts `Scanning` and turns `Ready` or
/// `Failed` when its scan ends; one from [`Sessions::get`] is built before
/// the call returns, so it is always `Ready`.
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

/// What a finished scan leaves: the diff and the memory it was given.
#[derive(Debug)]
pub(crate) struct Built {
    pub(crate) diff: Diff<FileSource, FileSource>,
    pub(crate) budget: Budget,
    pub(crate) pool: Pool,
}

/// A session's share of [`Limits::total_mem`], given back once: when the
/// scan fails or when the session is dropped.
#[derive(Debug)]
struct Held {
    total: Arc<AtomicU64>,
    mem: AtomicU64,
}

impl Held {
    fn release(&self) {
        // Relaxed: a counter that only bounds admission, read under the
        // state lock; a late decrement only makes room later.
        let mem = self.mem.swap(0, Ordering::Relaxed);
        self.total.fetch_sub(mem, Ordering::Relaxed);
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.release();
    }
}

/// The diff of one pair, or the scan building it, and the memory it was
/// given.
#[derive(Debug)]
pub struct Session {
    pub(crate) id: SessionId,
    pub(crate) mem: Bytes,
    pub(crate) bytes: [Bytes; 2],
    watch: Watch,
    built: OnceLock<Result<Built, Error>>,
    /// Set once the scan has let go of the session, after `built`: until
    /// then the session counts as scanning. Eviction takes every holder of
    /// a session for a call using it, so a ready one must be held by calls
    /// only.
    ended: Arc<OnceLock<()>>,
    held: Held,
    /// The [`Observed`] handles on the session. Each holds an `Arc` that
    /// eviction does not count as a use. Changed under the set's lock only.
    observers: AtomicUsize,
}

impl Session {
    /// Where the session is in its life.
    #[must_use]
    pub fn state(&self) -> SessionState {
        match self.result() {
            None => SessionState::Scanning,
            Some(Ok(_)) => SessionState::Ready,
            Some(Err(_)) => SessionState::Failed,
        }
    }

    /// Blocks until the scan ends and returns the state it left.
    pub fn wait(&self) -> SessionState {
        self.ended.wait();
        self.state()
    }

    /// The error the scan failed with, once it has.
    #[must_use]
    pub fn error(&self) -> Option<&Error> {
        self.result().and_then(|b| b.as_ref().err())
    }

    /// Bytes the scan has covered, and of how many: twice the size of both
    /// files, for its two passes. The first only grows, and equals the
    /// second once the scan has succeeded.
    #[must_use]
    pub fn scan_progress(&self) -> (Bytes, Bytes) {
        let total = Bytes(2 * (self.bytes[0].0 + self.bytes[1].0));
        (self.watch.done().min(total), total)
    }

    /// What the scan left, once it has ended.
    fn result(&self) -> Option<&Result<Built, Error>> {
        self.ended.get().and(self.built.get())
    }

    /// The finished diff.
    pub(crate) fn built(&self) -> Result<&Built, Error> {
        match self.result() {
            Some(Ok(b)) => Ok(b),
            _ => Err(Error::NotReady {
                session: self.id,
                state: self.state(),
            }),
        }
    }

    /// Scans `a` and `b` and builds the diff within the session's budget,
    /// with a response pool.
    fn build(&self, a: FileSource, b: FileSource) -> Result<Built, Error> {
        let mem = self.mem;
        let budget = Budget::new(mem);
        let build = |error| Error::Build { mem, error };
        // Taken before the diff, which sizes its workers and cache from
        // what the Refine half has left. Bytes the scheduler did not see
        // at construction would make its workers wait for them.
        let share =
            Bytes((budget.capacity(Half::Refine).0 / RESPONSE_SHARE).min(MAX_RESPONSE_POOL.0));
        let held = budget
            .try_reserve(Half::Refine, share)
            .map_err(|e| build(e.into()))?;
        let pool = Pool {
            budget: Budget::new(Bytes(2 * share.0)),
            _held: held,
        };
        let threads = std::thread::available_parallelism()
            .map_or(1, std::num::NonZero::get)
            .min(4);
        let diff = Diff::with_watch(
            Arc::new(a),
            Arc::new(b),
            &budget,
            threads,
            Config::default(),
            &self.watch,
        )
        .map_err(build)?;
        Ok(Built { diff, budget, pool })
    }
}

/// What [`Sessions::open`] returns.
#[derive(Debug)]
pub struct Opened {
    /// The session of the pair.
    pub session: Arc<Session>,
    /// It was live before the call, scanning or ready, and keeps the budget
    /// it was opened with.
    pub reused: bool,
}

#[derive(Debug)]
struct Entry {
    key: Key,
    session: Arc<Session>,
    used: Instant,
}

/// How long an `open` that found only observed sessions to evict waits for
/// an observer to let go. An observer reads one session for one tick of
/// notifications, microseconds of work.
const OBSERVER_WAIT: Duration = Duration::from_millis(250);

/// Why [`Sessions::make_room`] found no room.
enum Full {
    /// Every session that could be evicted is in use by a call.
    Busy,
    /// The sessions that could be evicted are held only by observers; room
    /// appears when one lets go.
    Observed,
}

/// Whether every holder of the session besides the table is an observer.
/// Read under the set's lock, which also guards every change to the
/// observer count and every new `Arc` of a live session.
fn only_observed(e: &Entry) -> bool {
    Arc::strong_count(&e.session) - e.session.observers.load(Ordering::Relaxed) == 1
}

/// A live session seen by [`Sessions::observe`].
#[derive(Debug)]
pub struct Observed<'s> {
    sessions: &'s Sessions,
    session: Option<Arc<Session>>,
}

impl std::ops::Deref for Observed<'_> {
    type Target = Session;

    fn deref(&self) -> &Session {
        self.session.as_ref().expect("held until drop")
    }
}

impl Drop for Observed<'_> {
    fn drop(&mut self) {
        let mut session = self.session.take();
        if let Some(s) = &session {
            // Uncounted and, while the table still holds the session,
            // released under the same lock: between the two the entry would
            // show one more holder than observers, which eviction takes for
            // a call in use. The table's own `Arc` keeps this one from being
            // the last, so no worker is joined under the lock.
            let state = self.sessions.lock();
            s.observers.fetch_sub(1, Ordering::Relaxed);
            if state.live.iter().any(|e| Arc::ptr_eq(&e.session, s)) {
                session = None;
            }
        }
        self.sessions.released.notify_all();
        // A session that left the table is released outside the lock: its
        // last holder drops the scheduler, which joins the refinement
        // workers.
        drop(session);
    }
}

#[derive(Debug, Default)]
struct State {
    live: Vec<Entry>,
    /// Sessions being built by [`Sessions::get`], which are not in `live`
    /// yet.
    building: usize,
    last_id: u64,
}

/// The sessions of a server.
#[derive(Debug)]
pub struct Sessions {
    state: Mutex<State>,
    /// Signalled when an [`Observed`] lets go, for an `open` waiting in
    /// [`Sessions::wait_observers`].
    released: Condvar,
    /// Budgets of every session that holds memory; see the module docs.
    held: Arc<AtomicU64>,
    limits: Limits,
}

/// Counts a session [`Sessions::get`] is building; uncounts it on drop.
struct Building<'s> {
    sessions: &'s Sessions,
}

impl Drop for Building<'_> {
    fn drop(&mut self) {
        self.sessions.lock().building -= 1;
    }
}

impl Sessions {
    /// An empty set under `limits`.
    #[must_use]
    pub fn new(limits: Limits) -> Self {
        Self {
            state: Mutex::default(),
            released: Condvar::new(),
            held: Arc::default(),
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

    /// Sum of the memory budgets of the sessions that hold memory: the
    /// live ones that have not failed, and the ones still winding down.
    #[must_use]
    pub fn mem(&self) -> Bytes {
        Bytes(self.held.load(Ordering::Relaxed))
    }

    /// The ids of the live sessions.
    #[must_use]
    pub fn ids(&self) -> Vec<SessionId> {
        self.lock().live.iter().map(|e| e.session.id).collect()
    }

    /// A handle for reporting on session `id`, if it is live. Unlike
    /// [`session`](Self::session) it does not mark the session as used. An
    /// `open` that needs a live session's room waits for the handle to be
    /// dropped, so a handle is held for a short read, never across a wait.
    /// A session closed while a handle holds it keeps its memory counted
    /// until the handle is dropped, and an `open` meanwhile answers busy.
    #[must_use]
    pub fn observe(&self, id: SessionId) -> Option<Observed<'_>> {
        let s = self.lock();
        let e = s.live.iter().find(|e| e.session.id == id)?;
        e.session.observers.fetch_add(1, Ordering::Relaxed);
        Some(Observed {
            sessions: self,
            session: Some(Arc::clone(&e.session)),
        })
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
    /// if none is live. `mem` is ignored for an existing session. Blocks
    /// for the scan, its own or that of a session [`open`](Self::open)
    /// started for the pair.
    ///
    /// A scan that fails leaves no session behind. `rift-mcp` renders its
    /// answers itself and uses this.
    ///
    /// # Errors
    ///
    /// [`Error::MemOverLimit`], [`Error::Busy`], or [`Error::Build`] with
    /// the core's error while scanning.
    pub fn get(
        &self,
        (a, id_a): (FileSource, FileId),
        (b, id_b): (FileSource, FileId),
        mem: Option<Bytes>,
    ) -> Result<Arc<Session>, Error> {
        let key = (id_a, id_b);
        let mem = self.check_mem(mem)?;
        let mut waited = None;
        let (session, _building) = loop {
            let mut s = self.lock();
            if let Some(i) = s.live.iter().position(|e| e.key == key) {
                let e = &mut s.live[i];
                if e.session.state() == SessionState::Failed {
                    // Replaced, as `open` would.
                    s.live.swap_remove(i);
                } else {
                    e.used = Instant::now();
                    let session = Arc::clone(&e.session);
                    drop(s);
                    if session.wait() == SessionState::Ready {
                        return Ok(session);
                    }
                    continue;
                }
            }
            match self.make_room(&mut s, mem) {
                Ok(()) => {}
                Err(Full::Observed) => {
                    drop(self.wait_observers(s, &mut waited)?);
                    continue;
                }
                Err(Full::Busy) => return Err(Error::Busy),
            }
            s.building += 1;
            let session = self.new_session(&mut s, mem, [a.len(), b.len()]);
            break (session, Building { sessions: self });
        };
        let built = session.build(a, b)?;
        // Never set before: the session was made by this call and is not
        // in the table yet.
        let _ = session.built.set(Ok(built));
        let _ = session.ended.set(());
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
        Ok(session)
    }

    /// Opens the files at `a` and `b` and returns their session.
    ///
    /// A live session of the pair is returned as it is, scanning or ready;
    /// a failed one is replaced. A new one is built with `mem` (the default
    /// when `None`) on a thread of its own: the call returns at once, with
    /// the session [`Scanning`](SessionState::Scanning).
    ///
    /// # Errors
    ///
    /// [`Error::File`] naming the side, [`Error::MemOverLimit`],
    /// [`Error::Busy`], or [`Error::Build`] when no thread can be started
    /// for the scan.
    pub fn open(&self, a: &str, b: &str, mem: Option<Bytes>) -> Result<Opened, Error> {
        let file = |side, path| FileSource::open(path).map_err(|error| Error::File { side, error });
        let (a, id_a) = file(Side::A, a)?;
        let (b, id_b) = file(Side::B, b)?;
        let key = (id_a, id_b);
        let mem = self.check_mem(mem)?;
        let mut waited = None;
        let session = loop {
            let mut s = self.lock();
            if let Some(e) = s.live.iter_mut().find(|e| e.key == key)
                && e.session.state() != SessionState::Failed
            {
                e.used = Instant::now();
                return Ok(Opened {
                    session: Arc::clone(&e.session),
                    reused: true,
                });
            }
            s.live.retain(|e| e.key != key);
            match self.make_room(&mut s, mem) {
                Ok(()) => {}
                Err(Full::Observed) => {
                    drop(self.wait_observers(s, &mut waited)?);
                    continue;
                }
                Err(Full::Busy) => return Err(Error::Busy),
            }
            let session = self.new_session(&mut s, mem, [a.len(), b.len()]);
            s.live.push(Entry {
                key,
                session: Arc::clone(&session),
                used: Instant::now(),
            });
            break session;
        };
        let scan = Arc::clone(&session);
        let spawned = std::thread::Builder::new()
            .name("rift-scan".into())
            .spawn(move || {
                let result = scan.build(a, b);
                if result.is_err() {
                    scan.held.release();
                }
                // Only this thread sets the result.
                let _ = scan.built.set(result);
                #[cfg(test)]
                drop(tests::SCAN_END.read());
                let ended = Arc::clone(&scan.ended);
                drop(scan);
                let _ = ended.set(());
            });
        if let Err(error) = spawned {
            self.close(session.id);
            return Err(Error::Build {
                mem,
                error: error.into(),
            });
        }
        Ok(Opened {
            session,
            reused: false,
        })
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

    /// Drops session `id` from the set and stops its scan if it is still
    /// scanning. Calls holding it finish first; its memory is released
    /// when the last of them does, or when the scan stops at its next
    /// read. Returns whether it was live.
    pub fn close(&self, id: SessionId) -> bool {
        let mut s = self.lock();
        let Some(i) = s.live.iter().position(|e| e.session.id == id) else {
            return false;
        };
        s.live.swap_remove(i).session.watch.cancel();
        true
    }

    /// Closes every session.
    pub fn close_all(&self) {
        for e in self.lock().live.drain(..) {
            e.session.watch.cancel();
        }
    }

    /// `mem`, or the default when `None`, if it is within the limit for one
    /// session.
    fn check_mem(&self, mem: Option<Bytes>) -> Result<Bytes, Error> {
        let mem = mem.unwrap_or(DEFAULT_MEM.min(self.limits.session_mem));
        if mem > self.limits.session_mem {
            return Err(Error::MemOverLimit {
                mem,
                limit: self.limits.session_mem,
            });
        }
        Ok(mem)
    }

    /// Evicts until a session of `mem` fits, as the module docs describe.
    /// A session an [`Observed`] still holds is never evicted: its memory
    /// would not be freed, and the next victim would be evicted for
    /// nothing.
    fn make_room(&self, s: &mut State, mem: Bytes) -> Result<(), Full> {
        let idle = self.limits.idle;
        s.live
            .retain(|e| Arc::strong_count(&e.session) > 1 || e.used.elapsed() < idle);
        loop {
            let held = self.held.load(Ordering::Relaxed);
            if s.live.len() + s.building < self.limits.sessions
                && held.saturating_add(mem.0) <= self.limits.total_mem.0
            {
                return Ok(());
            }
            let failed = s
                .live
                .iter()
                .position(|e| e.session.state() == SessionState::Failed);
            let ready = |e: &Entry| e.session.state() == SessionState::Ready;
            let lru = || {
                s.live
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| Arc::strong_count(&e.session) == 1 && ready(e))
                    .min_by_key(|(_, e)| e.used)
                    .map(|(i, _)| i)
            };
            let Some(i) = failed.or_else(lru) else {
                return Err(if s.live.iter().any(|e| ready(e) && only_observed(e)) {
                    Full::Observed
                } else {
                    Full::Busy
                });
            };
            s.live.swap_remove(i);
        }
    }

    /// Waits for an observer to let go, for [`OBSERVER_WAIT`] in all
    /// across the calls that share `waited`.
    fn wait_observers<'a>(
        &'a self,
        s: MutexGuard<'a, State>,
        waited: &mut Option<Instant>,
    ) -> Result<MutexGuard<'a, State>, Error> {
        let end = *waited.get_or_insert_with(|| Instant::now() + OBSERVER_WAIT);
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Error::Busy);
        }
        Ok(self
            .released
            .wait_timeout(s, left)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0)
    }

    /// A new session of `mem` for files of `bytes`, counted in the held
    /// memory; the caller has made room for it.
    fn new_session(&self, s: &mut State, mem: Bytes, bytes: [Bytes; 2]) -> Arc<Session> {
        self.held.fetch_add(mem.0, Ordering::Relaxed);
        s.last_id += 1;
        Arc::new(Session {
            id: SessionId(s.last_id),
            mem,
            bytes,
            watch: Watch::new(),
            built: OnceLock::new(),
            ended: Arc::default(),
            held: Held {
                total: Arc::clone(&self.held),
                mem: AtomicU64::new(mem.0),
            },
            observers: AtomicUsize::new(0),
        })
    }
}

#[cfg(fuzzing)]
#[doc(hidden)]
impl Session {
    pub fn fuzz_diff(&self) -> &Diff<FileSource, FileSource> {
        &self.built().expect("a session from get is ready").diff
    }
}

#[cfg(test)]
mod tests {
    use std::sync::RwLock;

    use super::*;

    /// Held for writing, stops every scan thread after it has set the
    /// result and before it lets go of the session.
    pub(super) static SCAN_END: RwLock<()> = RwLock::new(());

    fn write(dir: &std::path::Path, name: &str, bytes: &[u8]) -> String {
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        path.to_str().unwrap().to_owned()
    }

    #[test]
    fn a_session_is_ready_only_once_its_scan_thread_lets_go() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a", b"one\n");
        let b = write(dir.path(), "b", b"two\n");
        let c = write(dir.path(), "c", b"three\n");
        let s = Sessions::new(Limits {
            sessions: 1,
            ..Limits::default()
        });
        let pause = SCAN_END.write().unwrap();
        let session = s.open(&a, &b, None).unwrap().session;
        while session.built.get().is_none() {
            std::thread::yield_now();
        }
        // The scan thread still holds the session: eviction would take it
        // for a user, so it must not look ready yet.
        assert_eq!(session.state(), SessionState::Scanning);
        drop(pause);
        assert_eq!(session.wait(), SessionState::Ready);
        drop(session);
        s.open(&a, &c, None).unwrap();
    }

    /// Files `a` to `d` and a set under `total` with 16 MiB sessions.
    fn pairs(total: Bytes) -> (tempfile::TempDir, Sessions, [String; 4]) {
        let dir = tempfile::tempdir().unwrap();
        let f = |name: &str, text: &[u8]| write(dir.path(), name, text);
        let files = [
            f("a", b"one\n"),
            f("b", b"two\n"),
            f("c", b"three\n"),
            f("d", b"four\n"),
        ];
        let s = Sessions::new(Limits {
            sessions: 4,
            session_mem: Bytes::mib(16),
            total_mem: total,
            ..Limits::default()
        });
        (dir, s, files)
    }

    fn ready(s: &Sessions, a: &str, b: &str) -> SessionId {
        let session = s.open(a, b, Some(Bytes::mib(16))).unwrap().session;
        session.wait();
        session.id()
    }

    #[test]
    fn an_open_waits_for_the_observer_of_the_session_it_needs_room_from() {
        let (_dir, s, [a, b, c, _]) = pairs(Bytes::mib(16));
        let id = ready(&s, &a, &b);
        let looking = s.observe(id).unwrap();
        let opened = std::thread::scope(|scope| {
            let open = scope.spawn(|| s.open(&a, &c, Some(Bytes::mib(16))));
            drop(looking);
            open.join().unwrap()
        });
        assert!(opened.is_ok(), "{opened:?}");
        assert!(s.session(id).is_err(), "the old session made room");
    }

    #[test]
    fn eviction_skips_a_session_it_cannot_free() {
        // Two ready sessions, the first the least recently used and
        // observed: the second is evicted, the observed one stays.
        let (_dir, s, files) = pairs(Bytes::mib(32));
        let (first, second) = (
            ready(&s, &files[0], &files[1]),
            ready(&s, &files[0], &files[2]),
        );
        let looking = s.observe(first).unwrap();
        let opened = s.open(&files[0], &files[3], Some(Bytes::mib(16)));
        assert!(opened.is_ok(), "{opened:?}");
        assert!(s.session(first).is_ok(), "the observed session was evicted");
        assert!(s.session(second).is_err());
        drop(looking);
    }

    #[test]
    fn a_busy_open_leaves_the_observed_session_alone() {
        let (_dir, s, [a, b, c, _]) = pairs(Bytes::mib(16));
        let first = ready(&s, &a, &b);
        let looking = s.observe(first).unwrap();
        let opened = s.open(&a, &c, Some(Bytes::mib(16)));
        assert!(matches!(opened, Err(Error::Busy)), "{opened:?}");
        assert!(s.session(first).is_ok(), "busy, and the session is gone");
        drop(looking);
    }

    /// Observers come and go while opens need their sessions' room. An open
    /// waits for an observer to let go, so none may answer busy, and none
    /// may see the entry between the observer's uncount and its release.
    #[test]
    fn opens_beside_a_looping_observer_are_never_busy() {
        use std::sync::atomic::AtomicBool;
        const OPENS: usize = 2000;
        let (_dir, s, files) = pairs(Bytes::mib(16));
        let [a, b, c] = [&files[0], &files[1], &files[2]];
        ready(&s, a, b);
        let stop = AtomicBool::new(false);
        let busy = std::thread::scope(|scope| {
            scope.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    for id in s.ids() {
                        drop(s.observe(id));
                    }
                }
            });
            let mut busy = 0;
            for i in 0..OPENS {
                let other = if i % 2 == 0 { c } else { b };
                match s.open(a, other, Some(Bytes::mib(16))) {
                    Ok(opened) => {
                        opened.session.wait();
                    }
                    Err(Error::Busy) => busy += 1,
                    Err(err) => panic!("{err}"),
                }
            }
            stop.store(true, Ordering::Relaxed);
            busy
        });
        eprintln!("{busy} of {OPENS} opens were busy");
        assert_eq!(busy, 0);
    }
}
