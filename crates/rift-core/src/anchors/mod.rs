//! Anchor selection: pairs of samples, one per file, whose lines should
//! match.
//!
//! Candidates come from a [`CandidateSource`]. [`select`] is the one place
//! that decides which sources run: [`UniqueLines`], and [`Shingles`] for
//! data where few lines are unique, such as a table dump without an id
//! column. A source sees only the samples and the budget, and returns
//! pairs of sample indices, so the skeleton chains and confirms candidates
//! the same way whichever source found them.
//!
//! [`UniqueLines`] keeps line hashes that occur once in each file. The
//! samples of the file with fewer of them go into an open-addressing table
//! keyed by the line hash; the other file only probes it, since a hash
//! absent from the smaller side cannot be an anchor anyway. Each entry
//! records the sample index on each side, or that the hash occurred more
//! than once there. Entries unique on both sides become candidates.
//!
//! A candidate is a hash match, not yet a match. The skeleton confirms the
//! anchors it keeps byte by byte: most inside the streaming comparison of
//! the equal stretch they open, the rest by reading their two lines.
//! Reading every candidate here instead would cost one random read per
//! line and file, which on a pair larger than the page cache is a third
//! pass over the disk. A shingle anchor is confirmed the same way, by its
//! first line; the other lines of its window lie in the gaps that follow,
//! which are compared byte by byte or refined like any other.
//!
//! Memory, from the skeleton half: 16 bytes per table slot while the table
//! lives, between `8n / 7` and `2n` slots for `n` samples on the smaller
//! side, plus 8 bytes per such sample for the candidates, trimmed to the
//! candidate count once the table is gone. [`select`] runs the sources one
//! after the other, so only one table lives at a time, and holds the
//! candidates of both plus their merge while it merges them.

use crate::budget::{Budget, BudgetExceeded, Half, Reservation};
use crate::error::Result;
use crate::sample::Sample;
use crate::scan::release_slack;
use crate::types::Bytes;

mod shingles;

pub use shingles::Shingles;

/// A line unique in both files: the index of its sample in A and in B.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Pair {
    /// Index into the samples of A.
    pub a: u32,
    /// Index into the samples of B.
    pub b: u32,
}

const PAIR_BYTES: u64 = size_of::<Pair>() as u64;

/// Most occupied slots per slot, as a fraction `NUM / DEN`; past it linear
/// probing degrades and the caller should thin the samples instead.
const MAX_LOAD: (u64, u64) = (7, 8);

/// No sample in this slot.
const VACANT: u32 = u32::MAX;
/// The hash occurred more than once on this side.
const MANY: u32 = u32::MAX - 1;

#[derive(Debug, Clone, Copy)]
struct Entry {
    hash: u64,
    /// Sample index on the side with fewer samples, or a marker.
    small: u32,
    /// Sample index on the other side, or a marker.
    big: u32,
}

const ENTRY_BYTES: u64 = size_of::<Entry>() as u64;
const _: () = assert!(ENTRY_BYTES == 16);

/// Anchor candidates in increasing line order of A. Dropping it
/// releases the reservation.
#[derive(Debug)]
pub struct Anchors {
    pub(crate) pairs: Vec<Pair>,
    pub(crate) reservation: Reservation,
}

impl Anchors {
    /// The candidates, ordered by their line in A.
    #[must_use]
    pub fn pairs(&self) -> &[Pair] {
        &self.pairs
    }
}

/// Finds anchor candidates among the samples of two files.
///
/// An implementation returns [`Anchors`] whose pairs are ordered by `a`,
/// strictly increasing in `a`, with no `b` used twice, and whose
/// reservation, taken from [`Half::Skeleton`], covers the pairs it holds.
/// A candidate is a hash match: the skeleton confirms the ones it keeps
/// byte by byte, so a source never reads the files.
pub trait CandidateSource {
    /// Candidates among `samples_a` and `samples_b`.
    ///
    /// # Errors
    ///
    /// [`Error::Budget`](crate::Error::Budget) when the skeleton half cannot
    /// hold the source's working set; the caller thins the samples and
    /// tries again.
    fn select(
        &self,
        samples_a: &[Sample],
        samples_b: &[Sample],
        budget: &Budget,
    ) -> Result<Anchors>;
}

/// Candidates from every source in use, as the skeleton and the resampler
/// chain them: [`UniqueLines`], then [`Shingles`] of [`SHINGLE_W`]
/// samples from the gaps of at least [`SHINGLE_MIN_GAP`] samples that the
/// unique lines leave, merged in. When the shingles or the merge do not
/// fit the budget, the unique lines alone are returned.
///
/// # Errors
///
/// As for [`UniqueLines`].
pub fn select(samples_a: &[Sample], samples_b: &[Sample], budget: &Budget) -> Result<Anchors> {
    let mut lines = UniqueLines.select(samples_a, samples_b, budget)?;
    // On a budget error the unique lines are kept as they are.
    let _ = add_shingles(&mut lines, samples_a, samples_b, budget);
    Ok(lines)
}

/// Samples per shingle window.
pub const SHINGLE_W: usize = 3;

/// Fewest samples between two unique-line candidates, on either side, for
/// [`select`] to look for shingles there. Shorter gaps are refined cheaply
/// without anchors, and on data with an id column they hold under 1% of
/// the samples.
pub const SHINGLE_MIN_GAP: usize = 32;

/// Merges into `lines` the shingles of the gaps it leaves.
///
/// Memory, from the skeleton half: 8 bytes per candidate in `lines` for
/// the gap bounds, then as for [`Shingles`] and [`merge`].
fn add_shingles(lines: &mut Anchors, a: &[Sample], b: &[Sample], budget: &Budget) -> Result<()> {
    let windows = {
        let _cuts = budget.try_reserve(Half::Skeleton, Bytes(lines.pairs.len() as u64 * 8))?;
        let cuts_a: Vec<u32> = lines.pairs.iter().map(|p| p.a).collect();
        let mut cuts_b: Vec<u32> = lines.pairs.iter().map(|p| p.b).collect();
        cuts_b.sort_unstable();
        Shingles { w: SHINGLE_W }.in_gaps((a, &cuts_a), (b, &cuts_b), SHINGLE_MIN_GAP, budget)?
    };
    merge(lines, &windows, b.len(), budget)
}

/// Merges `extra` into `into` by A. Every pair of `into` stays; a pair of
/// `extra` is dropped when its A or B sample is already used, which for
/// two sources over the same samples takes a 64-bit hash collision. On a
/// budget error `into` is left as it was.
///
/// Memory, from the skeleton half: the merged list and one bit per sample
/// of B while it is built.
fn merge(into: &mut Anchors, extra: &Anchors, n_b: usize, budget: &Budget) -> Result<()> {
    if extra.pairs.is_empty() {
        return Ok(());
    }
    let len = into.pairs.len() + extra.pairs.len();
    let words = n_b.div_ceil(64);
    let mut reservation = budget.try_reserve(
        Half::Skeleton,
        Bytes(len as u64 * PAIR_BYTES + words as u64 * 8),
    )?;
    // `into`'s pairs already use distinct B samples; an extra pair is kept
    // only if its B sample is free.
    let mut taken = vec![0u64; words];
    let bit = |b: u32| (b as usize / 64, 1u64 << (b % 64));
    for p in &into.pairs {
        let (word, bit) = bit(p.b);
        taken[word] |= bit;
    }
    let mut merged = Vec::with_capacity(len);
    let mut extra = extra.pairs.iter().peekable();
    for &p in &into.pairs {
        while let Some(&&q) = extra.peek().filter(|q| q.a <= p.a) {
            extra.next();
            let (word, bit) = bit(q.b);
            if q.a < p.a && taken[word] & bit == 0 {
                taken[word] |= bit;
                merged.push(q);
            }
        }
        merged.push(p);
    }
    for &q in extra {
        let (word, bit) = bit(q.b);
        if taken[word] & bit == 0 {
            taken[word] |= bit;
            merged.push(q);
        }
    }
    drop(taken);
    reservation.shrink(Bytes(words as u64 * 8));
    into.pairs = merged;
    into.reservation = reservation;
    release_slack(&mut into.pairs, &mut into.reservation);
    Ok(())
}

/// Lines whose hash occurs exactly once among the samples of each file.
/// `O(n_a + n_b)` expected time for the table and `O(m log m)` to order
/// `m` candidates by A.
///
/// [`CandidateSource::select`] fails with
/// [`Error::Budget`](crate::Error::Budget) when the skeleton half cannot
/// hold the table at its highest load plus the candidates, or when a side
/// has more samples than a `u32` index can name; thinning the samples
/// fixes both.
#[derive(Debug, Clone, Copy, Default)]
pub struct UniqueLines;

impl CandidateSource for UniqueLines {
    fn select(
        &self,
        samples_a: &[Sample],
        samples_b: &[Sample],
        budget: &Budget,
    ) -> Result<Anchors> {
        unique_lines(samples_a, samples_b, budget)
    }
}

fn unique_lines(samples_a: &[Sample], samples_b: &[Sample], budget: &Budget) -> Result<Anchors> {
    unique_hashes(line_hashes(samples_a), line_hashes(samples_b), budget)
}

fn line_hashes(samples: &[Sample]) -> (usize, impl Iterator<Item = (usize, u64)> + '_) {
    (samples.len(), samples.iter().map(|s| s.hash).enumerate())
}

/// Pairs of sample indices whose hash occurs exactly once on each side, as
/// [`UniqueLines`] does for line hashes. Each side is a count and an
/// iterator that yields that many `(sample index, hash)` items, walked
/// once; the caller keeps the indices below `MANY`.
fn unique_hashes<I: Iterator<Item = (usize, u64)>>(
    side_a: (usize, I),
    side_b: (usize, I),
    budget: &Budget,
) -> Result<Anchors> {
    let swapped = side_b.0 < side_a.0;
    let (small, big) = if swapped {
        (side_b, side_a)
    } else {
        (side_a, side_b)
    };
    let n = small.0 as u64;
    let available = budget.available(Half::Skeleton).0;
    let min_slots = n * MAX_LOAD.1 / MAX_LOAD.0 + 1;
    let slots = (2 * n + 1).min(available.saturating_sub(n * PAIR_BYTES) / ENTRY_BYTES);
    if slots < min_slots || big.0 as u64 >= u64::from(MANY) {
        return Err(BudgetExceeded {
            half: Half::Skeleton,
            requested: Bytes(min_slots * ENTRY_BYTES + n * PAIR_BYTES),
            available: Bytes(available),
        }
        .into());
    }
    let table_bytes = slots * ENTRY_BYTES;
    let reservation = budget.try_reserve(Half::Skeleton, Bytes(table_bytes + n * PAIR_BYTES))?;

    let mut pairs = Vec::with_capacity(small.0);
    {
        let mut table = Table::new(slots);
        for (i, hash) in small.1 {
            table.count_small(hash, index(i));
        }
        for (i, hash) in big.1 {
            table.count_big(hash, index(i));
        }
        pairs.extend(table.unique().map(|(small, big)| {
            if swapped {
                Pair { a: big, b: small }
            } else {
                Pair { a: small, b: big }
            }
        }));
    }
    pairs.sort_unstable_by_key(|p| p.a);
    let mut anchors = Anchors { pairs, reservation };
    // The table's bytes stay reserved until the trim, which briefly holds
    // the old and the new block.
    release_slack(&mut anchors.pairs, &mut anchors.reservation);
    anchors.reservation.shrink(Bytes(table_bytes));
    Ok(anchors)
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "select checked both sample counts are below MANY"
)]
pub(crate) fn index(i: usize) -> u32 {
    i as u32
}

/// Open addressing with linear probing over a fixed slot count, keyed by
/// the line hash itself: it is already uniform, so the slot comes straight
/// from its high bits (the low bits are the stratum's zeros).
struct Table {
    slots: Vec<Entry>,
}

impl Table {
    fn new(slots: u64) -> Self {
        let vacant = Entry {
            hash: 0,
            small: VACANT,
            big: VACANT,
        };
        Self {
            slots: vec![vacant; usize::try_from(slots).expect("slot count fits usize")],
        }
    }

    /// The slot holding `hash`, or the vacant slot where it belongs. The
    /// load cap guarantees a vacant slot exists.
    fn find(&self, hash: u64) -> usize {
        let len = self.slots.len();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "the product shifted by 64 is below len"
        )]
        let mut i = ((u128::from(hash) * len as u128) >> 64) as usize;
        loop {
            let e = &self.slots[i];
            if e.small == VACANT || e.hash == hash {
                return i;
            }
            i += 1;
            if i == len {
                i = 0;
            }
        }
    }

    fn count_small(&mut self, hash: u64, i: u32) {
        let slot = self.find(hash);
        let e = &mut self.slots[slot];
        if e.small == VACANT {
            *e = Entry {
                hash,
                small: i,
                big: VACANT,
            };
        } else {
            e.small = MANY;
        }
    }

    fn count_big(&mut self, hash: u64, i: u32) {
        let slot = self.find(hash);
        let e = &mut self.slots[slot];
        if e.small != VACANT {
            e.big = if e.big == VACANT { i } else { MANY };
        }
    }

    /// `(small, big)` sample indices of every hash seen once on each side.
    fn unique(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.slots
            .iter()
            .filter(|e| e.small < MANY && e.big < MANY)
            .map(|e| (e.small, e.big))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ByteOffset, LineNo};

    /// Samples for every line of `data`, with hashes chosen by the test so
    /// collisions can be staged.
    fn samples(data: &[u8], hashes: &[u64]) -> Vec<Sample> {
        let mut offset = 0u64;
        let mut out = Vec::new();
        for (i, line) in data.split_inclusive(|&c| c == b'\n').enumerate() {
            out.push(Sample {
                hash: hashes[i],
                line: LineNo(i as u64),
                offset: ByteOffset(offset),
            });
            offset += line.len() as u64;
        }
        out
    }

    fn select_of(ha: &[u64], hb: &[u64]) -> Vec<Pair> {
        let line = |n: usize| "x\n".repeat(n).into_bytes();
        let (a, b) = (line(ha.len()), line(hb.len()));
        let budget = Budget::new(Bytes::mib(1));
        let got = UniqueLines
            .select(&samples(&a, ha), &samples(&b, hb), &budget)
            .unwrap();
        got.pairs().to_vec()
    }

    #[test]
    fn keeps_hashes_unique_in_both_files() {
        let pairs = select_of(&[1, 2, 2, 3, 4], &[3, 1, 5, 4, 4]);
        assert_eq!(pairs, [Pair { a: 0, b: 1 }, Pair { a: 3, b: 0 }]);
    }

    #[test]
    fn the_smaller_side_is_the_table_and_pairs_stay_in_a_order() {
        assert_eq!(
            select_of(&[1, 2, 3, 4], &[4, 2]),
            [Pair { a: 1, b: 1 }, Pair { a: 3, b: 0 }]
        );
        assert_eq!(select_of(&[], &[1]), []);
    }

    #[test]
    fn a_budget_too_small_for_the_table_is_reported() {
        let budget = Budget::new(Bytes(64));
        let s = samples(b"a\nb\n", &[1, 2]);
        let err = select(&s, &s, &budget).unwrap_err();
        assert!(matches!(err, crate::Error::Budget(_)));
    }

    #[test]
    fn shingles_fill_long_gaps_and_never_cost_a_failure() {
        // Unique lines, a short gap, more unique lines, then a long tail
        // where every value occurs twice but every window once.
        let tail = (0..40).map(|j| 1000 + j / 2);
        let h: Vec<u64> = (100..110)
            .chain([1, 2, 1, 2])
            .chain(110..120)
            .chain(tail)
            .collect();
        let data = "x\n".repeat(h.len()).into_bytes();
        let s = samples(&data, &h);
        let pair = |i| Pair { a: i, b: i };
        let unique: Vec<Pair> = (0..10).chain(14..24).map(pair).collect();
        let full: Vec<Pair> = (0..10).chain(14..62).map(pair).collect();
        let mut sizes = Vec::new();
        for bytes in (0..8192).step_by(8) {
            let budget = Budget::new(Bytes(bytes));
            let Ok(lines) = UniqueLines.select(&s, &s, &budget) else {
                continue;
            };
            assert_eq!(lines.pairs(), unique);
            drop(lines);
            let all = select(&s, &s, &budget).unwrap();
            assert!(all.pairs() == unique || all.pairs() == full, "{bytes} B");
            sizes.push(all.pairs().len());
        }
        assert_eq!(sizes.last(), Some(&full.len()));
    }

    #[test]
    fn a_merge_that_does_not_fit_leaves_the_candidates() {
        let budget = Budget::new(Bytes(64));
        let anchors = |pairs: &[(u32, u32)]| Anchors {
            pairs: pairs.iter().map(|&(a, b)| Pair { a, b }).collect(),
            reservation: budget.try_reserve(Half::Skeleton, Bytes(0)).unwrap(),
        };
        let mut into = anchors(&[(1, 1), (3, 3)]);
        let extra = anchors(&(4..12).map(|i| (i, i)).collect::<Vec<_>>());
        assert!(merge(&mut into, &extra, 12, &budget).is_err());
        assert_eq!(into.pairs(), [Pair { a: 1, b: 1 }, Pair { a: 3, b: 3 }]);
    }

    #[test]
    fn a_merged_pair_keeps_the_unique_line_on_a_clash() {
        let budget = Budget::new(Bytes::mib(1));
        let anchors = |pairs: &[(u32, u32)]| Anchors {
            pairs: pairs.iter().map(|&(a, b)| Pair { a, b }).collect(),
            reservation: budget.try_reserve(Half::Skeleton, Bytes(0)).unwrap(),
        };
        let mut into = anchors(&[(1, 1), (3, 3)]);
        let extra = anchors(&[(0, 0), (1, 1), (2, 3), (3, 5), (4, 4), (5, 1)]);
        merge(&mut into, &extra, 6, &budget).unwrap();
        let got: Vec<(u32, u32)> = into.pairs().iter().map(|p| (p.a, p.b)).collect();
        assert_eq!(got, [(0, 0), (1, 1), (3, 3), (4, 4)]);
    }
}
