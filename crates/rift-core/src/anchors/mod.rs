//! Anchor selection: pairs of samples, one per file, whose lines should
//! match.
//!
//! Candidates come from a [`CandidateSource`]. [`select`] is the one place
//! that decides which sources run; today that is [`UniqueLines`] alone.
//! A source sees only the samples and the budget, and returns pairs of
//! sample indices, so the skeleton chains and confirms candidates the same
//! way whichever source found them.
//!
//! [`UniqueLines`] keeps line hashes that occur once in each file. The samples of the file with fewer of them go into an open-addressing
//! table keyed by the line hash; the other file only probes it, since a
//! hash absent from the smaller side cannot be an anchor anyway. Each entry
//! records the sample index on each side, or that the hash occurred more
//! than once there. Entries unique on both sides become candidates.
//!
//! A candidate is a hash match, not yet a match. The skeleton confirms the
//! anchors it keeps byte by byte: most inside the streaming comparison of
//! the equal stretch they open, the rest by reading their two lines.
//! Reading every candidate here instead would cost one random read per
//! line and file, which on a pair larger than the page cache is a third
//! pass over the disk.
//!
//! Memory, from the skeleton half: 16 bytes per table slot while the table
//! lives, between `8n / 7` and `2n` slots for `n` samples on the smaller
//! side, plus 8 bytes per such sample for the candidates, trimmed to the
//! candidate count once the table is gone.

use crate::budget::{Budget, BudgetExceeded, Half, Reservation};
use crate::error::Result;
use crate::sample::Sample;
use crate::scan::release_slack;
use crate::types::Bytes;

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
/// chain them. Today that is [`UniqueLines`] alone.
///
/// # Errors
///
/// As for the sources' [`CandidateSource::select`].
pub fn select(samples_a: &[Sample], samples_b: &[Sample], budget: &Budget) -> Result<Anchors> {
    UniqueLines.select(samples_a, samples_b, budget)
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
    let swapped = samples_b.len() < samples_a.len();
    let (small, big) = if swapped {
        (samples_b, samples_a)
    } else {
        (samples_a, samples_b)
    };
    let n = small.len() as u64;
    let available = budget.available(Half::Skeleton).0;
    let min_slots = n * MAX_LOAD.1 / MAX_LOAD.0 + 1;
    let slots = (2 * n + 1).min(available.saturating_sub(n * PAIR_BYTES) / ENTRY_BYTES);
    if slots < min_slots || big.len() as u64 >= u64::from(MANY) {
        return Err(BudgetExceeded {
            half: Half::Skeleton,
            requested: Bytes(min_slots * ENTRY_BYTES + n * PAIR_BYTES),
            available: Bytes(available),
        }
        .into());
    }
    let table_bytes = slots * ENTRY_BYTES;
    let reservation = budget.try_reserve(Half::Skeleton, Bytes(table_bytes + n * PAIR_BYTES))?;

    let mut pairs = Vec::with_capacity(small.len());
    {
        let mut table = Table::new(slots);
        for (i, s) in small.iter().enumerate() {
            table.count_small(s.hash, index(i));
        }
        for (i, s) in big.iter().enumerate() {
            table.count_big(s.hash, index(i));
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
        let got = select(&samples(&a, ha), &samples(&b, hb), &budget).unwrap();
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
}
