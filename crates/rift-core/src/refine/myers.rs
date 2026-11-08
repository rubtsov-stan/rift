//! Linear-memory Myers (middle snake, divide and conquer) and the quadratic
//! DP used below [`DP_MAX`](super::DP_MAX).
//!
//! Coordinates are local to the gap: `x` in `0..=n`, `y` in `0..=m`,
//! diagonal `k = x - y` in `-m..=n`. Both furthest-reaching arrays hold `x`
//! per diagonal, indexed by `k + m + 1`, with [`NONE`] for a diagonal no
//! path reaches inside the box.

use std::ops::Range;

use super::{ApproxReason, Diff, LineHash, NONE};

impl Diff<'_> {
    /// Myers on `a[ra]` against `b[rb]` within the gap's `limit`. Recursion
    /// depth is `O(log D)`: each middle snake halves the edit distance.
    pub(super) fn myers(&mut self, ra: Range<usize>, rb: Range<usize>) {
        let Some((ra, rb)) = self.reduce(ra, rb) else {
            return;
        };
        let size = ra.len() + rb.len() + 3;
        let (vf, vb) = self.ws.scratch.split_at_mut(size);
        let split = middle_snake(
            &self.a[ra.clone()],
            &self.b[rb.clone()],
            vf,
            &mut vb[..size],
            self.limit,
            &mut self.spent,
        );
        if let Some((x, y)) = split {
            debug_assert!(
                (x, y) != (0, 0) && (x, y) != (ra.len(), rb.len()),
                "middle snake must split the box"
            );
            let (xa, yb) = (ra.start + x, rb.start + y);
            self.myers(ra.start..xa, rb.start..yb);
            self.myers(xa..ra.end, yb..rb.end);
        } else {
            // Cutoff: a whole replacement is valid, only not minimal.
            self.ws.removed[ra].fill(true);
            self.ws.added[rb].fill(true);
            self.approx = Some(ApproxReason::Cutoff);
        }
    }

    /// Suffix-LCS table, then a forward walk that marks the unmatched lines.
    /// `O(n·m)` time and cells; only called while `n + m <= DP_MAX`.
    #[allow(
        clippy::many_single_char_names,
        reason = "LCS table notation: n, m, w, t, i, j"
    )]
    pub(super) fn dp(&mut self, ra: Range<usize>, rb: Range<usize>) {
        let a = &self.a[ra.clone()];
        let b = &self.b[rb.clone()];
        let (n, m) = (a.len(), b.len());
        let w = m + 1;
        let t = &mut self.ws.scratch[..(n + 1) * w];
        t[n * w..].fill(0);
        for i in (0..n).rev() {
            t[i * w + m] = 0;
            for j in (0..m).rev() {
                t[i * w + j] = if a[i] == b[j] {
                    t[(i + 1) * w + j + 1] + 1
                } else {
                    t[(i + 1) * w + j].max(t[i * w + j + 1])
                };
            }
        }
        let (mut i, mut j) = (0, 0);
        while i < n && j < m {
            if a[i] == b[j] {
                i += 1;
                j += 1;
            } else if t[(i + 1) * w + j] >= t[i * w + j + 1] {
                self.ws.removed[ra.start + i] = true;
                i += 1;
            } else {
                self.ws.added[rb.start + j] = true;
                j += 1;
            }
        }
        self.ws.removed[ra.start + i..ra.end].fill(true);
        self.ws.added[rb.start + j..rb.end].fill(true);
    }
}

/// Finds a point on a shortest edit path through the box, or `None` when
/// `spent` would pass `limit`. `a` and `b` have no common prefix or suffix
/// and neither is empty.
///
/// `spent` counts diagonals visited plus snake steps, about one hash
/// comparison each.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::many_single_char_names,
    reason = "Myers notation; x, y, n, m are below u32::MAX by refine's check, k lies in -m..=n"
)]
fn middle_snake(
    a: &[LineHash],
    b: &[LineHash],
    vf: &mut [u32],
    vb: &mut [u32],
    limit: u64,
    spent: &mut u64,
) -> Option<(usize, usize)> {
    let (n, m) = (a.len() as i64, b.len() as i64);
    let delta = n - m;
    let off = m + 1;
    let at = |k: i64| (k + off) as usize;
    vf.fill(NONE);
    vb.fill(NONE);
    vf[at(0)] = 0;
    vb[at(delta)] = n as u32;
    let odd = delta & 1 != 0;

    for d in 1..=n + m {
        if *spent > limit {
            return None;
        }
        // Forward: diagonals k ≡ d (mod 2) inside the box.
        let (mut kmin, mut kmax) = ((-d).max(-m), d.min(n));
        kmin += (kmin - d) & 1;
        kmax -= (kmax - d) & 1;
        *spent += ((kmax - kmin) / 2 + 1) as u64;
        let mut k = kmax;
        while k >= kmin {
            let i = at(k);
            let (down, right) = (vf[i + 1], vf[i - 1]);
            let mut x = NONE;
            if down != NONE && i64::from(down) - (k + 1) < m {
                x = down;
            }
            if right != NONE && i64::from(right) < n && (x == NONE || right + 1 >= x) {
                x = right + 1;
            }
            if x == NONE {
                vf[i] = NONE;
            } else {
                let x0 = x as usize;
                let mut xi = x0;
                let mut y = (xi as i64 - k) as usize;
                while xi < a.len() && y < b.len() && a[xi] == b[y] {
                    xi += 1;
                    y += 1;
                }
                *spent += (xi - x0) as u64;
                vf[i] = xi as u32;
                if odd && vb[i] != NONE && vb[i] <= xi as u32 {
                    return Some((xi, y));
                }
            }
            k -= 2;
        }
        // Backward: diagonals k ≡ delta + d (mod 2) inside the box.
        let (mut kmin, mut kmax) = ((delta - d).max(-m), (delta + d).min(n));
        kmin += (kmin - delta - d) & 1;
        kmax -= (kmax - delta - d) & 1;
        *spent += ((kmax - kmin) / 2 + 1) as u64;
        let mut k = kmin;
        while k <= kmax {
            let i = at(k);
            let (left, up) = (vb[i + 1], vb[i - 1]);
            let mut x = NONE;
            if left != NONE && left >= 1 {
                x = left - 1;
            }
            if up != NONE && i64::from(up) >= k && (x == NONE || up < x) {
                x = up;
            }
            if x == NONE {
                vb[i] = NONE;
            } else {
                let x0 = x as usize;
                let mut xi = x0;
                let mut y = (xi as i64 - k) as usize;
                while xi > 0 && y > 0 && a[xi - 1] == b[y - 1] {
                    xi -= 1;
                    y -= 1;
                }
                *spent += (x0 - xi) as u64;
                vb[i] = xi as u32;
                if !odd && vf[i] != NONE && xi as u32 <= vf[i] {
                    return Some((xi, y));
                }
            }
            k += 2;
        }
    }
    debug_assert!(false, "a path of at most n + m edits always exists");
    None
}
