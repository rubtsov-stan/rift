//! Local diff of one segment: recursive patience, then linear-memory Myers.
//!
//! The segment arrives as two slices of line hashes. Lines unique in both
//! slices become anchors (patience); the gaps between anchors recurse, and
//! a gap without unique lines goes to Myers with a middle-snake search and
//! a cost cutoff. Tiny gaps use quadratic DP. The result is two flag arrays,
//! `removed` over A and `added` over B; the unflagged lines of both sides
//! pair up in order and are then confirmed byte by byte through
//! [`LineAccess`], since a hash match is not equality.
//!
//! Time is `O((N+M)·min(D, cutoff))` per gap plus `O((N+M) log (N+M))` per
//! patience level; memory is [`work_estimate`], about 17 bytes per line of
//! A plus B, reserved by the caller from the refine half of the budget.

mod myers;
#[cfg(test)]
mod tests;

use std::ops::Range;

use crate::budget::Reservation;
use crate::edit::{Edit, Hunk};
use crate::error::Result;
use crate::lines::{LineAccess, LineBuf};
use crate::segment::Segment;
use crate::types::{Bytes, LineHash};

/// Largest `N+M` handled by quadratic DP instead of Myers.
///
/// Chosen with `refine::tests::dp_threshold` (Apple M1, release, best of
/// 3): on 20 000 random gaps of 2 to 101 lines with 1 to 6 edits, thresholds
/// 4 to 12 run in 0.030 s against 0.031 s for Myers alone, within noise,
/// while a table sized for 128 lines cost 0.043 s at 64 and 0.059 s at 128.
/// On a 1M-line segment with 0.1% changes the threshold makes no difference
/// (0.070 s), since prefix and suffix stripping settles nearly every gap.
pub const DP_MAX: usize = 12;

/// Patience levels nested in one segment before falling back to Myers. Each
/// level strictly shrinks the gap; the guard only bounds the native stack
/// on adversarial input.
const MAX_DEPTH: usize = 32;

/// Lines per side read in one `read_lines` call while verifying matches.
const VERIFY_LINES: usize = 1024;

/// Not reached, in `u32` index arrays.
const NONE: u32 = u32::MAX;

/// Cells of the DP table: `(n+1)(m+1)` is largest at `n = m = DP_MAX / 2`.
const DP_CELLS: u64 = (DP_MAX as u64 / 2 + 1) * (DP_MAX as u64 / 2 + 1);

/// Bytes [`Refiner::refine`] allocates for a segment of `n` A lines and
/// `m` B lines, to reserve from the refine half before running.
///
/// The hunk returned is not included: its `Vec<Edit>` is sized exactly to
/// the number of runs once they are known and belongs to the hunk cache.
#[must_use]
pub fn work_estimate(n: u64, m: u64) -> Bytes {
    let s = n + m;
    let q = n.min(m);
    // scratch u32 words, pairs + anchors [u32; 2], two bool flag arrays.
    Bytes(scratch_words(s) * 4 + q * 16 + s)
}

/// `u32` words in the scratch buffer: records of 4 words per line during
/// the uniqueness sort, at least `2(n+m+3)` for both Myers arrays, and
/// the DP table.
fn scratch_words(s: u64) -> u64 {
    (4 * s + 8).max(DP_CELLS)
}

/// Per-worker state: the cost cutoff and the line buffers reused across
/// segments.
///
/// The line buffers hold up to [`VERIFY_LINES`] lines per side while a
/// match is verified and are not part of [`work_estimate`]: their size
/// follows line length, which the estimate does not know. The caller
/// accounts for them once per worker, like its read buffers.
#[derive(Debug)]
pub struct Refiner {
    cutoff: u64,
    // Fixed outside tests, which switch patience off and sweep the DP size.
    dp_max: usize,
    patience: bool,
    buf_a: LineBuf,
    buf_b: LineBuf,
}

impl Refiner {
    /// A refiner that explores at most `cutoff` edits per gap.
    ///
    /// The Myers search on a gap of `n+m` lines with edit distance `D`
    /// costs about `(n+m)·D` hash comparisons; beyond `(n+m)·cutoff` the
    /// gap is emitted as a whole replacement and the hunk is marked
    /// `approx`. `git` uses roughly `sqrt(n+m)`, with a floor of 256.
    #[must_use]
    pub fn new(cutoff: u64) -> Self {
        Self {
            cutoff,
            dp_max: DP_MAX,
            patience: true,
            buf_a: LineBuf::new(),
            buf_b: LineBuf::new(),
        }
    }

    /// Diffs one segment.
    ///
    /// `a` and `b` are the hashes of the segment's lines, `lines_a` and
    /// `lines_b` give their bytes for verification, and `reservation` should
    /// hold [`work_estimate`] of the two lengths: a smaller one is grown.
    ///
    /// # Errors
    ///
    /// `Budget` if the reservation is smaller than the estimate and cannot
    /// grow, `Io` or `LineOutOfRange` from the line sources.
    ///
    /// # Panics
    ///
    /// Panics if the slice lengths do not match the segment ranges or a
    /// side has `u32::MAX` lines or more.
    pub fn refine<L: LineAccess + ?Sized>(
        &mut self,
        seg: &Segment,
        a: &[LineHash],
        b: &[LineHash],
        lines_a: &L,
        lines_b: &L,
        reservation: &mut Reservation,
    ) -> Result<Hunk> {
        assert_eq!(
            a.len() as u64,
            seg.len_a(),
            "A hashes must cover the segment"
        );
        assert_eq!(
            b.len() as u64,
            seg.len_b(),
            "B hashes must cover the segment"
        );
        assert!(
            a.len() < NONE as usize && b.len() < NONE as usize,
            "a segment side must have fewer than u32::MAX lines"
        );
        let estimate = work_estimate(a.len() as u64, b.len() as u64);
        reservation.grow(Bytes(estimate.0.saturating_sub(reservation.bytes().0)))?;
        let mut ws = Workspace::new(a.len(), b.len())?;
        debug_assert_eq!(ws.bytes(), estimate, "workspace must match work_estimate");

        let mut diff = Diff {
            a,
            b,
            ws: &mut ws,
            cutoff: self.cutoff,
            dp_max: self.dp_max,
            patience: self.patience,
            limit: 0,
            spent: 0,
            approx: false,
        };
        diff.solve(0..a.len(), 0..b.len(), 0);
        let approx = diff.approx;

        verify(
            &mut ws,
            seg,
            lines_a,
            lines_b,
            &mut self.buf_a,
            &mut self.buf_b,
        )?;
        let edits = build_edits(&ws, seg)?;
        Ok(Hunk { edits, approx })
    }
}

/// Buffers for one refinement, allocated once from the reservation.
struct Workspace {
    /// Records for the uniqueness sort, then LIS `prev`/`tails`, then both
    /// Myers arrays or the DP table; never two at once.
    scratch: Vec<u32>,
    /// `(ia, ib)` of lines unique in both sides of the current level.
    pairs: Vec<[u32; 2]>,
    /// Anchor chains of all active patience levels, stacked.
    anchors: Vec<[u32; 2]>,
    removed: Vec<bool>,
    added: Vec<bool>,
}

impl Workspace {
    fn new(n: usize, m: usize) -> Result<Self> {
        let s = n + m;
        let q = n.min(m);
        Ok(Self {
            scratch: filled(Bytes(scratch_words(s as u64)).as_usize(), 0)?,
            pairs: filled(q, [0, 0])?,
            anchors: filled(q, [0, 0])?,
            removed: filled(n, false)?,
            added: filled(m, false)?,
        })
    }

    fn bytes(&self) -> Bytes {
        let b = self.scratch.capacity() * 4
            + (self.pairs.capacity() + self.anchors.capacity()) * 8
            + self.removed.capacity()
            + self.added.capacity();
        Bytes(b as u64)
    }
}

/// A vector of exactly `len` copies of `v`, without panicking on OOM.
fn filled<T: Clone>(len: usize, v: T) -> Result<Vec<T>> {
    let mut out = Vec::new();
    out.try_reserve_exact(len).map_err(oom)?;
    out.resize(len, v);
    Ok(out)
}

fn oom(_: std::collections::TryReserveError) -> crate::Error {
    std::io::Error::from(std::io::ErrorKind::OutOfMemory).into()
}

/// One refinement in progress; positions are indices into `a` and `b`.
struct Diff<'w> {
    a: &'w [LineHash],
    b: &'w [LineHash],
    ws: &'w mut Workspace,
    cutoff: u64,
    dp_max: usize,
    patience: bool,
    /// Comparisons the current gap may spend in Myers, and spent so far.
    limit: u64,
    spent: u64,
    approx: bool,
}

impl Diff<'_> {
    /// Diffs `a[ra]` against `b[rb]` at patience depth `depth`.
    fn solve(&mut self, ra: Range<usize>, rb: Range<usize>, depth: usize) {
        let Some((ra, rb)) = self.reduce(ra, rb) else {
            return;
        };
        if self.patience
            && depth < MAX_DEPTH
            && let Some(chain) = self.anchors(ra.clone(), rb.clone())
        {
            let (mut pa, mut pb) = (ra.start, rb.start);
            for i in chain.clone() {
                let [x, y] = self.ws.anchors[i];
                let (x, y) = (x as usize, y as usize);
                self.solve(pa..x, pb..y, depth + 1);
                (pa, pb) = (x + 1, y + 1);
            }
            self.solve(pa..ra.end, pb..rb.end, depth + 1);
            self.ws.anchors.truncate(chain.start);
            return;
        }
        self.limit = ((ra.len() + rb.len()) as u64).saturating_mul(self.cutoff);
        self.spent = 0;
        self.myers(ra, rb);
    }

    /// Drops the hash-equal prefix and suffix, which stay unflagged, and
    /// settles the gap when a side is empty or it is small enough for DP.
    /// Returns what is left for the caller to diff.
    pub(super) fn reduce(
        &mut self,
        ra: Range<usize>,
        rb: Range<usize>,
    ) -> Option<(Range<usize>, Range<usize>)> {
        let (ra, rb) = self.strip(ra, rb);
        if ra.is_empty() || rb.is_empty() {
            self.ws.removed[ra].fill(true);
            self.ws.added[rb].fill(true);
            return None;
        }
        if ra.len() + rb.len() <= self.dp_max {
            self.dp(ra, rb);
            return None;
        }
        Some((ra, rb))
    }

    fn strip(&self, mut ra: Range<usize>, mut rb: Range<usize>) -> (Range<usize>, Range<usize>) {
        let prefix = self.a[ra.clone()]
            .iter()
            .zip(&self.b[rb.clone()])
            .take_while(|(x, y)| x == y)
            .count();
        ra.start += prefix;
        rb.start += prefix;
        let suffix = self.a[ra.clone()]
            .iter()
            .rev()
            .zip(self.b[rb.clone()].iter().rev())
            .take_while(|(x, y)| x == y)
            .count();
        ra.end -= suffix;
        rb.end -= suffix;
        (ra, rb)
    }

    /// Pushes the longest increasing chain of lines unique in both ranges
    /// onto the anchor stack and returns its slot range, or `None` if there
    /// is no unique line.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::many_single_char_names,
        reason = "side lengths are checked below u32::MAX in refine; a, b, s, p follow the algorithm"
    )]
    fn anchors(&mut self, ra: Range<usize>, rb: Range<usize>) -> Option<Range<usize>> {
        let a = &self.a[ra.clone()];
        let b = &self.b[rb.clone()];
        let s = a.len() + b.len();
        let (recs, _) = self.ws.scratch[..4 * s].as_chunks_mut::<4>();
        let rec = |h: LineHash, i: usize, side: u32| [h as u32, (h >> 32) as u32, i as u32, side];
        for (r, (i, &h)) in recs.iter_mut().zip(a.iter().enumerate()) {
            *r = rec(h, i, 0);
        }
        for (r, (j, &h)) in recs[a.len()..].iter_mut().zip(b.iter().enumerate()) {
            *r = rec(h, j, 1);
        }
        recs.sort_unstable_by_key(|r| (r[1], r[0]));

        let pairs = &mut self.ws.pairs;
        pairs.clear();
        for group in recs.chunk_by(|x, y| x[..2] == y[..2]) {
            if let [x, y] = group
                && x[3] != y[3]
            {
                let (ia, ib) = if x[3] == 0 {
                    (x[2], y[2])
                } else {
                    (y[2], x[2])
                };
                pairs.push([ia, ib]);
            }
        }
        if pairs.is_empty() {
            return None;
        }
        // Into A order by scattering over the dead records, cheaper than
        // a sort: `scratch[ia] = ib`.
        let (slots, rest) = self.ws.scratch.split_at_mut(a.len());
        slots.fill(NONE);
        for pr in pairs.iter() {
            slots[pr[0] as usize] = pr[1];
        }
        pairs.clear();
        for (ia, &ib) in slots.iter().enumerate() {
            if ib != NONE {
                pairs.push([ia as u32, ib]);
            }
        }

        // Patience sorting: `tails[k]` is the pair ending the shortest
        // increasing chain of length k+1, `prev` links each pair back.
        let p = pairs.len();
        let (prev, rest) = rest.split_at_mut(p);
        let tails = &mut rest[..p];
        let mut len = 0;
        for (i, pr) in pairs.iter().enumerate() {
            // Nearly every pair extends the chain on real data, so test the
            // tail first and binary search only when it does not.
            let k = if len > 0 && pairs[tails[len - 1] as usize][1] < pr[1] {
                len
            } else {
                tails[..len].partition_point(|&t| pairs[t as usize][1] < pr[1])
            };
            prev[i] = if k > 0 { tails[k - 1] } else { NONE };
            tails[k] = i as u32;
            if k == len {
                len += 1;
            }
        }
        let anchors = &mut self.ws.anchors;
        let base = anchors.len();
        anchors.resize(base + len, [0, 0]);
        let mut cur = tails[len - 1];
        for slot in (base..base + len).rev() {
            let pr = pairs[cur as usize];
            anchors[slot] = [pr[0] + ra.start as u32, pr[1] + rb.start as u32];
            cur = prev[cur as usize];
        }
        Some(base..base + len)
    }
}

/// Confirms every unflagged pair byte by byte, flagging the pairs whose
/// bytes differ as a one-line replacement. That leaves the script valid
/// and possibly not minimal without `approx`, which means cost: a 64-bit
/// hash collision is a `2^-64` event.
fn verify<L: LineAccess + ?Sized>(
    ws: &mut Workspace,
    seg: &Segment,
    lines_a: &L,
    lines_b: &L,
    buf_a: &mut LineBuf,
    buf_b: &mut LineBuf,
) -> Result<()> {
    let (removed, added) = (&mut ws.removed, &mut ws.added);
    let (n, m) = (removed.len(), added.len());
    let (mut i, mut j) = (0, 0);
    loop {
        while i < n && removed[i] {
            i += 1;
        }
        while j < m && added[j] {
            j += 1;
        }
        if i >= n || j >= m {
            break;
        }
        // The block ends before the first pair that would push either side
        // past VERIFY_LINES, so every line is read once.
        let (i0, j0) = (i, j);
        let (mut ie, mut je) = (i, j);
        let (mut ci, mut cj) = (i, j);
        loop {
            while ci < n && removed[ci] {
                ci += 1;
            }
            while cj < m && added[cj] {
                cj += 1;
            }
            if ci >= n || cj >= m || ci - i0 >= VERIFY_LINES || cj - j0 >= VERIFY_LINES {
                break;
            }
            (ie, je) = (ci + 1, cj + 1);
            (ci, cj) = (ci + 1, cj + 1);
        }
        lines_a.read_lines(seg.a.start + i0 as u64..seg.a.start + ie as u64, buf_a)?;
        lines_b.read_lines(seg.b.start + j0 as u64..seg.b.start + je as u64, buf_b)?;
        while i < ie && j < je {
            if removed[i] {
                i += 1;
            } else if added[j] {
                j += 1;
            } else {
                if buf_a.line(i - i0) != buf_b.line(j - j0) {
                    removed[i] = true;
                    added[j] = true;
                }
                i += 1;
                j += 1;
            }
        }
    }
    debug_assert!(i >= n && j >= m, "unflagged lines must pair up exactly");
    Ok(())
}

/// Turns the flag arrays into ordered runs with absolute line numbers.
fn build_edits(ws: &Workspace, seg: &Segment) -> Result<Vec<Edit>> {
    let mut runs = 0;
    walk_runs(ws, seg, &mut |_| runs += 1);
    let mut edits = Vec::new();
    edits.try_reserve_exact(runs).map_err(oom)?;
    walk_runs(ws, seg, &mut |e| edits.push(e));
    Ok(edits)
}

/// Calls `sink` once per run, in order, with absolute line numbers.
fn walk_runs(ws: &Workspace, seg: &Segment, sink: &mut impl FnMut(Edit)) {
    let (removed, added) = (&ws.removed, &ws.added);
    let (n, m) = (removed.len(), added.len());
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && removed[i] {
            let start = i;
            while i < n && removed[i] {
                i += 1;
            }
            sink(Edit::Delete {
                a: seg.a.start + start as u64,
                len: (i - start) as u64,
            });
        } else if j < m && added[j] {
            let start = j;
            while j < m && added[j] {
                j += 1;
            }
            sink(Edit::Insert {
                b: seg.b.start + start as u64,
                len: (j - start) as u64,
            });
        } else {
            let (si, sj) = (i, j);
            while i < n && j < m && !removed[i] && !added[j] {
                i += 1;
                j += 1;
            }
            debug_assert!(i > si, "unflagged lines must pair up exactly");
            sink(Edit::Equal {
                a: seg.a.start + si as u64,
                b: seg.b.start + sj as u64,
                len: (i - si) as u64,
            });
        }
    }
}
