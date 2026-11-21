//! A hold on scans, for tests that must know a scan is still in progress
//! when they act. Built only with the `test-hooks` feature, which only
//! dev-dependencies enable.
//!
//! The hold is a file next to A, so it also reaches a scan in a
//! `rift serve` child process. A held scan keeps a copy of A's path and
//! the hold file's name, outside any budget; neither exists without the
//! feature.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// While alive, holds every scan whose A was opened by the path given,
/// once it has read both files and before it ends: the session stays
/// [`Scanning`](crate::SessionState::Scanning) with all its bytes counted
/// in [`scan_progress`](crate::Session::scan_progress).
#[derive(Debug)]
pub struct ScanHold(String);

impl ScanHold {
    /// Holds the scans of the pairs whose A is `a`.
    ///
    /// # Panics
    ///
    /// If the hold file next to `a` cannot be written.
    #[must_use]
    pub fn new(a: &str) -> Self {
        let path = hold_path(a);
        std::fs::write(&path, b"").expect("the hold file is writable");
        Self(path)
    }
}

impl Drop for ScanHold {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn hold_path(a: &str) -> String {
    format!("{a}.hold")
}

/// Blocks while a [`ScanHold`] on `a` is alive.
pub(crate) fn wait(a: &str) {
    let path = hold_path(a);
    while Path::new(&path).exists() {
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Live [`RefineHold`]s, so a read looks for a hold file only while one
/// exists.
static REFINE_HOLDS: AtomicU64 = AtomicU64::new(0);
/// Reads a [`RefineHold`] is holding now.
static HELD_READS: AtomicU64 = AtomicU64::new(0);

/// While alive, holds every read that a refinement worker makes of the file
/// opened by the path given, so a test can change the file after the scan
/// and before refinement reads it.
#[derive(Debug)]
pub struct RefineHold(String);

impl RefineHold {
    /// Holds refinement's reads of `path`.
    ///
    /// # Panics
    ///
    /// If the hold file next to `path` cannot be written.
    #[must_use]
    pub fn new(path: &str) -> Self {
        let hold = format!("{path}.refine-hold");
        std::fs::write(&hold, b"").expect("the hold file is writable");
        REFINE_HOLDS.fetch_add(1, Ordering::SeqCst);
        Self(hold)
    }

    /// Whether a refinement read waits on a hold now: the scan has ended
    /// and refinement has begun. Counts the reads of every live hold.
    #[must_use]
    pub fn holding(&self) -> bool {
        HELD_READS.load(Ordering::SeqCst) > 0
    }
}

impl Drop for RefineHold {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        REFINE_HOLDS.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Blocks a refinement worker's read of `path` while a [`RefineHold`] on
/// it is alive.
pub(crate) fn refine_read(path: &str) {
    if REFINE_HOLDS.load(Ordering::SeqCst) == 0
        || std::thread::current().name() != Some("rift-sched")
    {
        return;
    }
    let hold = format!("{path}.refine-hold");
    HELD_READS.fetch_add(1, Ordering::SeqCst);
    while Path::new(&hold).exists() {
        std::thread::sleep(Duration::from_millis(5));
    }
    HELD_READS.fetch_sub(1, Ordering::SeqCst);
}

static READS: AtomicU64 = AtomicU64::new(0);
static READ_BYTES: AtomicU64 = AtomicU64::new(0);

pub(crate) fn count_read(bytes: usize) {
    READS.fetch_add(1, Ordering::Relaxed);
    READ_BYTES.fetch_add(bytes as u64, Ordering::Relaxed);
}

/// `pread` calls and bytes they returned, on every file the sessions of
/// this process have opened, since the process began. A test that diffs two
/// readings must be the only one running in its binary.
#[must_use]
pub fn reads() -> (u64, u64) {
    (
        READS.load(Ordering::Relaxed),
        READ_BYTES.load(Ordering::Relaxed),
    )
}
