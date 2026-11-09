//! Refinement of a segment whose estimate exceeds what one job may reserve.
//!
//! The segment's lines are sampled again, inside the segment only, at the
//! lowest stratum whose samples fit, and lines unique among those samples
//! on both sides are chained by the longest increasing subsequence, as the
//! skeleton does for the whole files. The chain cuts the segment into
//! gaps. A gap that fits is refined; one that does not is compared byte by
//! byte when both sides have the same line count, and otherwise resampled
//! at a lower stratum still. Only a gap that is not equal and has no
//! anchor at stratum 0, or none below its parent's stratum, becomes a
//! whole replacement, and the hunk is marked approximate. The cuts need no
//! confirmation: any cut gives a valid script, because each gap's own
//! script is exact.
//!
//! Memory: everything comes out of the job's reservation, split by a
//! private [`Budget`] of the same size. Its skeleton half holds the samples,
//! the anchor table and the cuts of every level in progress; its refine
//! half holds the edits produced so far (half of it, fixed) and one gap
//! refinement at a time.

use std::ops::Range;

use super::{Out, Worker, each_hash, estimate, refine, same_lines};
use crate::anchors;
use crate::budget::{Budget, Half, Reservation};
use crate::edit::{ApproxReason, Edit, Hunk};
use crate::error::{Error, Result};
use crate::sample::Sample;
use crate::sched::Cancel;
use crate::segment::Segment;
use crate::skeleton::longest_increasing;
use crate::source::Source;
use crate::strata::{self, MAX_LEVEL};
use crate::types::{ByteOffset, Bytes, LineHash, LineNo};

const SAMPLE_BYTES: u64 = size_of::<Sample>() as u64;
/// Skeleton-half bytes per sample while a level is chained: the record, at
/// most two anchor table slots of 16 bytes, a candidate pair and the
/// subsequence search, rounded up.
const BYTES_PER_SAMPLE: u64 = 80;

/// Refines `seg` within `limit` bytes.
pub(super) fn refine_large<A: Source + Send + ?Sized, B: Source + Send + ?Sized>(
    w: &mut Worker<A, B>,
    seg: &Segment,
    limit: Bytes,
    cancel: &Cancel,
) -> Result<Hunk> {
    let inner = Budget::new(limit);
    let edits = Bytes(inner.capacity(Half::Refine).0 / 2);
    let Ok(mut out) = Out::new(&inner, Half::Refine, edits, (seg.a.start, seg.b.start)) else {
        // Too small for anything but the whole replacement, which the
        // scheduler guarantees fits.
        return Ok(whole(seg));
    };
    let done = match split(
        w,
        &inner,
        seg.a.clone(),
        seg.b.clone(),
        MAX_LEVEL + 1,
        &mut out,
        cancel,
    ) {
        // The inner budget is this job's own: running out of it makes an
        // approximate hunk, not a refusal of --mem, whose numbers it does
        // not share.
        Err(Error::Budget(_)) => return Ok(whole(seg)),
        other => other?,
    };
    if !done && !cancel.is_set() {
        let rest = (out.at_a..seg.a.end, out.at_b..seg.b.end);
        let closed = out.replace(rest.0, rest.1, 0, ApproxReason::Budget)?;
        debug_assert!(closed, "two slots are kept for the rest");
    }
    Ok(out.into_hunk())
}

/// The segment as one approximate replacement.
fn whole(seg: &Segment) -> Hunk {
    let mut edits = Vec::with_capacity(2);
    if seg.len_a() > 0 {
        edits.push(Edit::Delete {
            a: seg.a.start,
            len: seg.len_a(),
        });
    }
    if seg.len_b() > 0 {
        edits.push(Edit::Insert {
            b: seg.b.start,
            len: seg.len_b(),
        });
    }
    Hunk {
        edits,
        approx: Some(ApproxReason::Budget),
    }
}

/// Appends the script of A lines `ra` against B lines `rb` to `out`,
/// resampling below stratum `parent` when they do not fit. False when
/// `out` is full or the job was cancelled; the caller closes the rest.
fn split<A: Source + Send + ?Sized, B: Source + Send + ?Sized>(
    w: &mut Worker<A, B>,
    inner: &Budget,
    ra: Range<LineNo>,
    rb: Range<LineNo>,
    parent: u32,
    out: &mut Out,
    cancel: &Cancel,
) -> Result<bool> {
    if cancel.is_set() {
        return Ok(false);
    }
    let (n, m) = (ra.end - ra.start, rb.end - rb.start);
    if n == 0 || m == 0 {
        let del = n == 0
            || out.push(Edit::Delete {
                a: ra.start,
                len: n,
            })?;
        return Ok(del
            && (m == 0
                || out.push(Edit::Insert {
                    b: rb.start,
                    len: m,
                })?));
    }
    let need = estimate(n, m);
    let small = n < u64::from(u32::MAX) && m < u64::from(u32::MAX);
    if small && need <= inner.available(Half::Refine) {
        let mut reservation = inner.try_reserve(Half::Refine, need)?;
        let hunk = refine(w, ra, rb, None, &mut reservation, cancel)?;
        drop(reservation);
        out.approx = out.approx.max(hunk.approx);
        for e in hunk.edits {
            if !out.push(e)? {
                return Ok(false);
            }
        }
        return Ok(true);
    }
    if n == m && same_lines(w, ra.clone(), rb.clone())? {
        return out.push(Edit::Equal {
            a: ra.start,
            b: rb.start,
            len: n,
        });
    }
    let chain = match chain(w, inner, &ra, &rb, parent)? {
        Ok(chain) => chain,
        Err(NoChain::Disjoint) => {
            return Ok(out.push(Edit::Delete {
                a: ra.start,
                len: n,
            })? && out.push(Edit::Insert {
                b: rb.start,
                len: m,
            })?);
        }
        Err(NoChain::Approx(reason)) => return out.replace(ra, rb, 2, reason),
    };
    let (mut at_a, mut at_b) = (ra.start, rb.start);
    for &(x, y) in &chain.cuts {
        if !split(w, inner, at_a..x, at_b..y, chain.level, out, cancel)? {
            return Ok(false);
        }
        (at_a, at_b) = (x, y);
    }
    split(
        w,
        inner,
        at_a..ra.end,
        at_b..rb.end,
        chain.level,
        out,
        cancel,
    )
}

/// Cut points inside a range: pairs of lines unique among the samples of
/// both sides, increasing on both.
struct Chain {
    cuts: Vec<(LineNo, LineNo)>,
    level: u32,
    _reservation: Reservation,
}

/// Why a range has no chain.
enum NoChain {
    /// No line hash is on both sides, so no line is, and the whole
    /// replacement is minimal.
    Disjoint,
    /// The range stays approximate, for this reason.
    Approx(ApproxReason),
}

/// Samples A lines `ra` and B lines `rb` at the lowest stratum below
/// `parent` whose samples fit the skeleton half of `inner`, and chains
/// them. The inner error says why there is no chain.
///
/// The same read proves the sides disjoint when no line hash of one is on
/// the other: the shorter side is read first into a [`Seen`], and every
/// line of the longer one is looked up in it. The `Seen` is released before
/// the anchor search, which sees the budget it would without it. When it
/// does not fit, a range without anchors stays approximate for lack of
/// budget.
fn chain<A: Source + Send + ?Sized, B: Source + Send + ?Sized>(
    w: &mut Worker<A, B>,
    inner: &Budget,
    ra: &Range<LineNo>,
    rb: &Range<LineNo>,
    parent: u32,
) -> Result<std::result::Result<Chain, NoChain>> {
    // Half of what is free, so the levels below this one have room for
    // theirs while this chain is held.
    let cap = inner.available(Half::Skeleton).0 / 2 / BYTES_PER_SAMPLE;
    let lines = (ra.end - ra.start) + (rb.end - rb.start);
    let mut level = strata::initial_level(lines, cap);
    if cap == 0 || level >= parent {
        return Ok(Err(NoChain::Approx(ApproxReason::Budget)));
    }
    let cap = Bytes(cap).as_usize();
    let held = inner.try_reserve(Half::Skeleton, Bytes(cap as u64 * SAMPLE_BYTES))?;
    let (n, m) = (ra.end - ra.start, rb.end - rb.start);
    let a_first = n <= m;
    let mut seen = Seen::new(inner, n.min(m), n.max(m));
    let checked = seen.is_some();
    let mut samples: Vec<Sample> = Vec::with_capacity(cap);
    // Samples of the side read first come first; the final set does not
    // depend on the order, so B first only needs a rotation after.
    let mut in_first = 0;
    let order = if a_first { [0, 1] } else { [1, 0] };
    for (k, side) in order.into_iter().enumerate() {
        let range = if side == 0 { ra } else { rb };
        if k == 1 {
            in_first = samples.len();
        }
        let mut line = range.start;
        let mut sample = |hash| {
            if let Some(s) = &mut seen {
                s.see(k == 0, hash);
            }
            if level < parent && strata::contains(level, hash) {
                // Copies of one line share a hash: a level may keep all of
                // them, so thin until there is room or no level is left.
                while samples.len() >= cap && level < parent {
                    level += 1;
                    in_first = thin(&mut samples, in_first, level);
                }
                if level < parent && strata::contains(level, hash) {
                    samples.push(Sample {
                        hash,
                        line,
                        offset: ByteOffset(0),
                    });
                }
            }
            line += 1;
        };
        if side == 0 {
            each_hash(&w.a, range.clone(), &mut w.buf, &mut sample)?;
        } else {
            each_hash(&w.b, range.clone(), &mut w.buf, &mut sample)?;
        }
    }
    if seen.take().is_some_and(Seen::disjoint) {
        return Ok(Err(NoChain::Disjoint));
    }
    let mut in_a = in_first;
    if !a_first {
        samples.rotate_left(in_first);
        in_a = samples.len() - in_first;
    }
    let anchors = loop {
        if level >= parent {
            return Ok(Err(NoChain::Approx(ApproxReason::Budget)));
        }
        let (sa, sb) = samples.split_at(in_a);
        let attempt = anchors::select(sa, sb, inner).and_then(|mut found| {
            longest_increasing(&mut found, inner, |_| {})?;
            Ok(found)
        });
        match attempt {
            Ok(found) => break found,
            Err(Error::Budget(_)) => {
                level += 1;
                in_a = thin(&mut samples, in_a, level);
            }
            Err(e) => return Err(e),
        }
    };
    let pairs = anchors.pairs();
    if pairs.is_empty() {
        let reason = if checked {
            ApproxReason::NoAnchor
        } else {
            ApproxReason::Budget
        };
        return Ok(Err(NoChain::Approx(reason)));
    }
    let reservation = inner.try_reserve(Half::Skeleton, Bytes(16 * pairs.len() as u64))?;
    let (sa, sb) = samples.split_at(in_a);
    let cuts = pairs
        .iter()
        .map(|p| (sa[p.a as usize].line, sb[p.b as usize].line))
        .collect();
    drop(anchors);
    drop(samples);
    drop(held);
    Ok(Ok(Chain {
        cuts,
        level,
        _reservation: reservation,
    }))
}

/// Line hashes of the side of a range read first, to tell whether any
/// line of the other side has one of them.
///
/// Looking each line up in a sorted or bucketed copy of 1.3M hashes costs
/// a cache miss per line. Instead each first-side hash sets two bits of
/// one word in a filter of 16 bits per hash, which stays in cache, and a
/// second-side hash is kept as a candidate only when both its bits are
/// set: about 2% of hashes that are not there, and every one that is. At
/// the end one pass over the first side's hashes looks them up among the
/// sorted candidates, behind the same kind of filter rebuilt over the
/// candidates in the first one's words. A candidate found there is a
/// shared hash. More
/// candidates than the room for them prove nothing, and the range goes on
/// as if a hash were shared.
///
/// Memory: 8 bytes per line of the first side for the hashes, 2 for the
/// filter, and 8 bytes per 32 lines of the second side for candidates,
/// from the skeleton half of the job's budget, held only while chain
/// reads.
struct Seen {
    hashes: Vec<LineHash>,
    /// Two bits per hash in one word, chosen by its low 44 bits: a bit
    /// that is not set proves the hash absent.
    filter: Vec<u64>,
    candidates: Vec<LineHash>,
    /// More candidates came than `candidates` has room for.
    overflow: bool,
    _reservation: Reservation,
}

impl Seen {
    /// Room for `first` hashes and the candidates among `second`, or none
    /// when it does not fit.
    fn new(inner: &Budget, first: u64, second: u64) -> Option<Self> {
        let words = first / 4 + 1;
        let room = second / 32 + 64;
        let bytes = 8 * (first + words + room);
        let reservation = inner.try_reserve(Half::Skeleton, Bytes(bytes)).ok()?;
        let vec = |len: u64| -> Option<Vec<LineHash>> {
            let mut v = Vec::new();
            v.try_reserve_exact(usize::try_from(len).ok()?).ok()?;
            Some(v)
        };
        let mut filter = vec(words)?;
        filter.resize(usize::try_from(words).ok()?, 0);
        Some(Self {
            hashes: vec(first)?,
            filter,
            candidates: vec(room)?,
            overflow: false,
            _reservation: reservation,
        })
    }

    /// Keeps `h` while the first side is read, filters it after.
    fn see(&mut self, first: bool, h: LineHash) {
        let (w, bits) = self.slot(h);
        if first {
            self.hashes.push(h);
            self.filter[w] |= bits;
        } else if self.filter[w] & bits == bits && !self.overflow {
            if self.candidates.len() < self.candidates.capacity() {
                self.candidates.push(h);
            } else {
                self.overflow = true;
            }
        }
    }

    /// Whether no hash of the second side is among the first side's.
    fn disjoint(mut self) -> bool {
        if self.overflow {
            return false;
        }
        if self.candidates.is_empty() {
            return true;
        }
        self.candidates.sort_unstable();
        // The same filter shape, now over the candidates; its words are
        // reused from the first side's filter, which is no longer needed.
        let words = (self.candidates.capacity() / 4).min(self.filter.len());
        self.filter.truncate(words);
        self.filter.fill(0);
        for i in 0..self.candidates.len() {
            let (w, bits) = self.slot(self.candidates[i]);
            self.filter[w] |= bits;
        }
        !self.hashes.iter().any(|&h| {
            let (w, bits) = self.slot(h);
            self.filter[w] & bits == bits && self.candidates.binary_search(&h).is_ok()
        })
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "the product of two 32-bit values shifted by 32 is below the word count"
    )]
    fn slot(&self, h: LineHash) -> (usize, u64) {
        let w = ((h & 0xffff_ffff) * self.filter.len() as u64) >> 32;
        (w as usize, 1 << ((h >> 32) & 63) | 1 << ((h >> 38) & 63))
    }
}

/// Keeps the samples of stratum `level` and returns how many of the first
/// `in_first` remain.
fn thin(samples: &mut Vec<Sample>, in_first: usize, level: u32) -> usize {
    let kept_a = samples[..in_first]
        .iter()
        .filter(|s| strata::contains(level, s.hash))
        .count();
    strata::thin(samples, level);
    kept_a
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    proptest! {
        /// A second side is disjoint exactly when it shares no hash with
        /// the first, through filter hits and misses alike.
        #[test]
        fn seen_is_disjoint_exactly_without_a_shared_hash(
            first in prop::collection::vec(any::<u64>(), 0..3000),
            other in prop::collection::vec(any::<u64>(), 0..200),
            shared in prop::option::of(any::<prop::sample::Index>()),
        ) {
            let mut second = other.clone();
            if let (Some(i), false) = (shared, first.is_empty()) {
                second.push(first[i.index(first.len())]);
            }
            let len = |v: &[u64]| v.len() as u64;
            let budget = Budget::new(Bytes::mib(4));
            let mut seen = Seen::new(&budget, len(&first), len(&second)).unwrap();
            for &h in &first {
                seen.see(true, h);
            }
            for &h in &second {
                seen.see(false, h);
            }
            let truth = !second.iter().any(|h| first.contains(h));
            prop_assert_eq!(seen.disjoint(), truth);
        }
    }
}
