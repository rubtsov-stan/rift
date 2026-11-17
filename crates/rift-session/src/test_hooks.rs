//! A hold on scans, for tests that must know a scan is still in progress
//! when they act. Built only with the `test-hooks` feature, which only
//! dev-dependencies enable.
//!
//! The hold is a file next to A, so it also reaches a scan in a
//! `rift serve` child process. A held scan keeps a copy of A's path and
//! the hold file's name, outside any budget; neither exists without the
//! feature.

use std::path::Path;
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
