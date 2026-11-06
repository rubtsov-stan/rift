//! In-memory `DiffView` for tests and the example. The real one arrives
//! with the scheduler integration.
#![allow(dead_code, missing_docs, clippy::too_many_lines)]

use std::ops::Range;
use std::sync::Arc;
use std::sync::Mutex;

use rift_core::moves::MovedBlock;
use rift_core::{DiffView, Edit, Hunk, HunkState, LineNo, Progress, Segment, SegmentStatus};

#[derive(Clone, Copy)]
pub enum Spec {
    /// Lines both files share.
    Equal(u64),
    /// Delete `del` lines of A, keep `mid` shared lines, insert `ins` lines of B.
    Diff { del: u64, mid: u64, ins: u64 },
    /// Same as `Diff`, but the local diff was cut short.
    Approx { del: u64, mid: u64, ins: u64 },
}

struct State {
    shown: Vec<HunkState>,
    done: u64,
}

pub struct MockView {
    len_a: u64,
    len_b: u64,
    segments: Vec<Segment>,
    truth: Vec<HunkState>,
    state: Mutex<State>,
    total: u64,
    pub visible: Mutex<Vec<Range<LineNo>>>,
    /// Segments passed to `hunk`, recorded once `log_hunk_calls` is called.
    calls: Mutex<Option<Vec<usize>>>,
    /// What `moved_blocks` returns; none unless a test sets it.
    pub moved: Vec<MovedBlock>,
}

pub struct Mock {
    pub view: MockView,
    pub a: Vec<u8>,
    pub b: Vec<u8>,
}

impl Mock {
    /// `refined`: whether every hunk is ready from the start.
    pub fn build(specs: &[Spec], refined: bool) -> Self {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        let (mut na, mut nb) = (0u64, 0u64);
        let (mut segments, mut truth) = (Vec::new(), Vec::new());
        let shared = |a: &mut Vec<u8>, b: &mut Vec<u8>, na: &mut u64, nb: &mut u64, n| {
            for _ in 0..n {
                let l = format!("line {:07} the quick brown fox\n", *na);
                a.extend_from_slice(l.as_bytes());
                b.extend_from_slice(l.as_bytes());
                *na += 1;
                *nb += 1;
            }
        };
        for spec in specs {
            let (sa, sb) = (na, nb);
            match *spec {
                Spec::Equal(n) => {
                    shared(&mut a, &mut b, &mut na, &mut nb, n);
                    segments.push(Segment {
                        a: LineNo(sa)..LineNo(na),
                        b: LineNo(sb)..LineNo(nb),
                        status: SegmentStatus::Equal,
                    });
                    truth.push(HunkState::Ready(Arc::new(Hunk::default())));
                }
                Spec::Diff { del, mid, ins } | Spec::Approx { del, mid, ins } => {
                    let approx = matches!(spec, Spec::Approx { .. });
                    for _ in 0..del {
                        a.extend_from_slice(format!("line {na:07} was removed here\n").as_bytes());
                        na += 1;
                    }
                    shared(&mut a, &mut b, &mut na, &mut nb, mid);
                    for _ in 0..ins {
                        b.extend_from_slice(format!("line {nb:07} was added here\n").as_bytes());
                        nb += 1;
                    }
                    let mut edits = Vec::new();
                    if del > 0 {
                        edits.push(Edit::Delete {
                            a: LineNo(sa),
                            len: del,
                        });
                    }
                    if mid > 0 {
                        edits.push(Edit::Equal {
                            a: LineNo(sa + del),
                            b: LineNo(sb),
                            len: mid,
                        });
                    }
                    if ins > 0 {
                        edits.push(Edit::Insert {
                            b: LineNo(sb + mid),
                            len: ins,
                        });
                    }
                    segments.push(Segment {
                        a: LineNo(sa)..LineNo(na),
                        b: LineNo(sb)..LineNo(nb),
                        status: if approx {
                            SegmentStatus::Approx
                        } else {
                            SegmentStatus::Differs
                        },
                    });
                    let h = Arc::new(Hunk { edits, approx });
                    truth.push(if approx {
                        HunkState::Approx(h)
                    } else {
                        HunkState::Ready(h)
                    });
                }
            }
        }
        let total = segments
            .iter()
            .filter(|s| s.status != SegmentStatus::Equal)
            .count() as u64;
        let shown = segments
            .iter()
            .zip(&truth)
            .map(|(s, t)| {
                if refined || s.status == SegmentStatus::Equal {
                    t.clone()
                } else {
                    HunkState::Pending
                }
            })
            .collect();
        let view = MockView {
            len_a: na,
            len_b: nb,
            segments,
            truth,
            state: Mutex::new(State {
                shown,
                done: if refined { total } else { 0 },
            }),
            total,
            visible: Mutex::new(Vec::with_capacity(1 << 16)),
            calls: Mutex::new(None),
            moved: Vec::new(),
        };
        Self { view, a, b }
    }

    /// `n` repetitions of an equal stretch followed by a changed one, with
    /// every fifth changed segment approximate.
    pub fn large(n: usize, refined: bool) -> Self {
        let mut specs = Vec::with_capacity(n * 2);
        for i in 0..n {
            specs.push(Spec::Equal(20 + (i as u64 * 7) % 60));
            let (del, mid, ins) = (1 + (i as u64 % 4), (i as u64 % 3) * 2, i as u64 % 5);
            specs.push(if i % 5 == 4 {
                Spec::Approx { del, mid, ins }
            } else {
                Spec::Diff { del, mid, ins }
            });
        }
        Self::build(&specs, refined)
    }
}

impl MockView {
    /// Forgets a refined hunk, as the cache does on eviction.
    pub fn evict(&self, segment: usize) {
        let mut st = self.state.lock().unwrap();
        st.shown[segment] = HunkState::Pending;
        st.done -= 1;
    }

    /// Starts recording which segments `hunk` is asked for.
    pub fn log_hunk_calls(&self) {
        *self.calls.lock().unwrap() = Some(Vec::new());
    }

    pub fn hunk_calls(&self) -> Vec<usize> {
        self.calls.lock().unwrap().clone().unwrap_or_default()
    }

    /// Finishes one pending hunk, the one closest after the visible window.
    /// Returns whether anything was pending.
    pub fn refine_next(&self) -> bool {
        let vis = self.visible.lock().unwrap().last().cloned();
        let mut st = self.state.lock().unwrap();
        let start = vis.map_or(0, |r| r.start.0);
        let pick = {
            let pending = |i: &usize| matches!(st.shown[*i], HunkState::Pending);
            let from = self.segments.partition_point(|s| s.a.end.0 <= start);
            (from..self.segments.len()).chain(0..from).find(pending)
        };
        let Some(i) = pick else { return false };
        st.shown[i] = self.truth[i].clone();
        st.done += 1;
        true
    }
}

impl DiffView for MockView {
    fn len_a(&self) -> u64 {
        self.len_a
    }
    fn len_b(&self) -> u64 {
        self.len_b
    }
    fn segments(&self) -> &[Segment] {
        &self.segments
    }
    fn hunk(&self, segment: usize) -> HunkState {
        if let Some(log) = self.calls.lock().unwrap().as_mut() {
            log.push(segment);
        }
        self.state.lock().unwrap().shown[segment].clone()
    }
    fn progress(&self) -> Progress {
        Progress {
            done: self.state.lock().unwrap().done,
            total: self.total,
        }
    }
    fn set_visible(&self, range: Range<LineNo>) {
        self.visible.lock().unwrap().push(range);
    }
    fn moved_blocks(&self) -> &[MovedBlock] {
        &self.moved
    }
}
