//! LRU cache of finished hunks: one row per segment, index links, no `Box`.
//!
//! The row table is allocated once for every segment and doubles as the
//! scheduler's per-segment state. Cached rows form a doubly linked list
//! through `prev`/`next` indices; the tail is the least recently used.

use std::sync::Arc;

use crate::edit::{Edit, Hunk};
use crate::types::Bytes;

const NONE: u32 = u32::MAX;

/// Where a segment is in its life cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum State {
    /// Confirmed equal by the skeleton; never needs a job.
    Equal,
    /// Needs a job: never computed, evicted, or cancelled.
    Idle,
    /// A worker is on it.
    Running,
    /// The hunk is in the cache.
    Cached,
    /// The job panicked; asking for the hunk panics too.
    Failed,
}

#[derive(Debug)]
struct Row {
    prev: u32,
    next: u32,
    hunk: Option<Arc<Hunk>>,
    state: State,
    /// Completed at least once; stays set across eviction for progress.
    done: bool,
}

const _: () = assert!(size_of::<Row>() == 24);

/// Heap bytes a cached hunk holds besides its edits: the `Arc` header and
/// the struct.
pub const HUNK_OVERHEAD: Bytes = Bytes((2 * size_of::<usize>() + size_of::<Hunk>()) as u64);

/// Heap bytes a cached hunk holds: the overhead and the edits.
pub(super) fn hunk_bytes(hunk: &Hunk) -> u64 {
    HUNK_OVERHEAD.0 + (hunk.edits.capacity() * size_of::<Edit>()) as u64
}

#[derive(Debug)]
pub(super) struct Cache {
    rows: Vec<Row>,
    head: u32,
    tail: u32,
    bytes: u64,
    cap: u64,
    /// Segments completed at least once.
    done: u64,
}

fn idx(i: usize) -> u32 {
    u32::try_from(i).expect("segment count checked against u32::MAX at construction")
}

impl Cache {
    /// Bytes the row table takes for `n` segments.
    pub(super) fn table_bytes(n: usize) -> Bytes {
        Bytes((n * size_of::<Row>()) as u64)
    }

    /// A cache for `n` segments holding at most `cap` bytes of hunks.
    ///
    /// # Panics
    ///
    /// Panics if `n` does not fit a `u32` index.
    pub(super) fn new(n: usize, equal: impl Fn(usize) -> bool, cap: Bytes) -> Self {
        assert!(n < NONE as usize, "segment count exceeds u32 index space");
        let rows = (0..n)
            .map(|i| Row {
                prev: NONE,
                next: NONE,
                hunk: None,
                state: if equal(i) { State::Equal } else { State::Idle },
                done: false,
            })
            .collect();
        Cache {
            rows,
            head: NONE,
            tail: NONE,
            bytes: 0,
            cap: cap.0,
            done: 0,
        }
    }

    /// Segments in the table.
    pub(super) fn len(&self) -> usize {
        self.rows.len()
    }

    pub(super) fn state(&self, i: usize) -> State {
        self.rows[i].state
    }

    pub(super) fn set_state(&mut self, i: usize, state: State) {
        debug_assert!(state != State::Cached, "Cached is set by insert");
        self.rows[i].state = state;
    }

    /// Needs its first computation: never completed, or cancelled.
    pub(super) fn fresh(&self, i: usize) -> bool {
        let row = &self.rows[i];
        row.state == State::Idle && !row.done
    }

    /// Records a completion; only the first one per segment counts, and
    /// returns whether this was it.
    pub(super) fn mark_done(&mut self, i: usize) -> bool {
        let first = !std::mem::replace(&mut self.rows[i].done, true);
        self.done += u64::from(first);
        first
    }

    /// Segments completed at least once, even if evicted since.
    pub(super) fn done(&self) -> u64 {
        self.done
    }

    /// Bytes of hunks held.
    #[cfg(test)]
    pub(super) fn bytes(&self) -> u64 {
        self.bytes
    }

    /// The cached hunk, made most recently used.
    pub(super) fn get(&mut self, i: usize) -> Option<Arc<Hunk>> {
        let hunk = self.rows[i].hunk.clone()?;
        let i = idx(i);
        if self.head != i {
            self.unlink(i);
            self.link_front(i);
        }
        Some(hunk)
    }

    /// Caches `hunk` for segment `i`, evicting from the LRU end until it fits.
    ///
    /// Returns false, leaving the row `Idle`, when the hunk alone exceeds
    /// the capacity.
    pub(super) fn insert(&mut self, i: usize, hunk: Arc<Hunk>) -> bool {
        debug_assert!(self.rows[i].hunk.is_none(), "segment cached twice");
        let need = hunk_bytes(&hunk);
        if need > self.cap {
            self.rows[i].state = State::Idle;
            return false;
        }
        while self.bytes + need > self.cap {
            self.evict_lru();
        }
        let row = &mut self.rows[i];
        row.hunk = Some(hunk);
        row.state = State::Cached;
        self.bytes += need;
        self.link_front(idx(i));
        true
    }

    fn evict_lru(&mut self) {
        let i = self.tail;
        debug_assert!(i != NONE, "eviction on an empty cache");
        self.unlink(i);
        let row = &mut self.rows[i as usize];
        let hunk = row.hunk.take().expect("linked row holds a hunk");
        row.state = State::Idle;
        self.bytes -= hunk_bytes(&hunk);
    }

    fn link_front(&mut self, i: u32) {
        let row = &mut self.rows[i as usize];
        row.prev = NONE;
        row.next = self.head;
        if self.head == NONE {
            self.tail = i;
        } else {
            self.rows[self.head as usize].prev = i;
        }
        self.head = i;
    }

    fn unlink(&mut self, i: u32) {
        let Row { prev, next, .. } = self.rows[i as usize];
        if prev == NONE {
            self.head = next;
        } else {
            self.rows[prev as usize].next = next;
        }
        if next == NONE {
            self.tail = prev;
        } else {
            self.rows[next as usize].prev = prev;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LineNo;

    fn hunk(n: usize) -> Arc<Hunk> {
        Arc::new(Hunk {
            edits: vec![
                Edit::Delete {
                    a: LineNo(0),
                    len: 1
                };
                n
            ],
            approx: None,
        })
    }

    #[test]
    fn evicts_least_recently_used_first() {
        let one = hunk_bytes(&hunk(1));
        let mut c = Cache::new(4, |_| false, Bytes(2 * one));
        assert!(c.insert(0, hunk(1)));
        assert!(c.insert(1, hunk(1)));
        assert!(c.get(0).is_some(), "touch makes 0 the most recent");
        assert!(c.insert(2, hunk(1)));
        assert_eq!(c.state(1), State::Idle, "1 was the LRU");
        assert_eq!(c.state(0), State::Cached);
        assert_eq!(c.state(2), State::Cached);
        assert_eq!(c.bytes(), 2 * one);
        assert!(c.insert(3, hunk(1)));
        assert_eq!(c.state(0), State::Idle);
        assert!(c.get(3).is_some() && c.get(2).is_some() && c.get(0).is_none());
    }

    #[test]
    fn oversized_hunk_is_refused_and_evicts_nothing() {
        let one = hunk_bytes(&hunk(1));
        let mut c = Cache::new(2, |_| false, Bytes(one));
        assert!(c.insert(0, hunk(1)));
        assert!(!c.insert(1, hunk(2)));
        assert_eq!(c.state(0), State::Cached);
        assert_eq!(c.state(1), State::Idle);
    }
}
