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
use crate::edit::{Edit, Hunk};
use crate::error::{Error, Result};
use crate::sample::Sample;
use crate::sched::Cancel;
use crate::segment::Segment;
use crate::skeleton::longest_increasing;
use crate::source::Source;
use crate::strata::{self, MAX_LEVEL};
use crate::types::{ByteOffset, Bytes, LineNo};

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
    let done = split(
        w,
        &inner,
        seg.a.clone(),
        seg.b.clone(),
        MAX_LEVEL + 1,
        &mut out,
        cancel,
    )?;
    if !done && !cancel.is_set() {
        let rest = (out.at_a..seg.a.end, out.at_b..seg.b.end);
        let closed = out.replace(rest.0, rest.1, 0)?;
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
        approx: true,
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
        out.approx |= hunk.approx;
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
    let Some(chain) = chain(w, inner, &ra, &rb, parent)? else {
        return out.replace(ra, rb, 2);
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

/// Samples A lines `ra` and B lines `rb` at the lowest stratum below
/// `parent` whose samples fit the skeleton half of `inner`, and chains
/// them. `None` when no stratum below `parent` fits or no anchor is found.
fn chain<A: Source + Send + ?Sized, B: Source + Send + ?Sized>(
    w: &mut Worker<A, B>,
    inner: &Budget,
    ra: &Range<LineNo>,
    rb: &Range<LineNo>,
    parent: u32,
) -> Result<Option<Chain>> {
    // Half of what is free, so the levels below this one have room for
    // theirs while this chain is held.
    let cap = inner.available(Half::Skeleton).0 / 2 / BYTES_PER_SAMPLE;
    let lines = (ra.end - ra.start) + (rb.end - rb.start);
    let mut level = strata::initial_level(lines, cap);
    if cap == 0 || level >= parent {
        return Ok(None);
    }
    let cap = Bytes(cap).as_usize();
    let held = inner.try_reserve(Half::Skeleton, Bytes(cap as u64 * SAMPLE_BYTES))?;
    let mut samples: Vec<Sample> = Vec::with_capacity(cap);
    let mut in_a = 0;
    for (side, range) in [(0, ra), (1, rb)] {
        let mut line = range.start;
        let mut sample = |hash| {
            if level < parent && strata::contains(level, hash) {
                if samples.len() == cap {
                    level += 1;
                    in_a = thin(&mut samples, in_a, level);
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
            in_a = samples.len();
        } else {
            each_hash(&w.b, range.clone(), &mut w.buf, &mut sample)?;
        }
    }
    let anchors = loop {
        if level >= parent {
            return Ok(None);
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
        return Ok(None);
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
    Ok(Some(Chain {
        cuts,
        level,
        _reservation: reservation,
    }))
}

/// Keeps the samples of stratum `level`, A's first, and returns how many
/// of A's remain.
fn thin(samples: &mut Vec<Sample>, in_a: usize, level: u32) -> usize {
    let kept_a = samples[..in_a]
        .iter()
        .filter(|s| strata::contains(level, s.hash))
        .count();
    strata::thin(samples, level);
    kept_a
}
