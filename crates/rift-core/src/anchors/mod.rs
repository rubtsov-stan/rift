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

use std::ops::Range;

use rayon::prelude::*;

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
/// A's gaps come from the candidates in their order, B's from a bitmap of
/// the B samples they use, so neither side is copied or sorted, and when
/// no gap on either side is long enough the shingles cost one walk over
/// the candidates.
///
/// Memory, from the skeleton half: one bit per sample of B, 16 bytes per
/// gap long enough, then as for [`Shingles`] and [`merge`].
fn add_shingles(lines: &mut Anchors, a: &[Sample], b: &[Sample], budget: &Budget) -> Result<()> {
    let min = SHINGLE_MIN_GAP.max(SHINGLE_W);
    let gaps_a = |f: &mut dyn FnMut(Range<usize>)| {
        long_gaps(lines.pairs.iter().map(|p| p.a as usize), a.len(), min, f);
    };
    let mut count_a = 0;
    gaps_a(&mut |_| count_a += 1);
    if count_a == 0 {
        return Ok(());
    }
    let words = b.len().div_ceil(64);
    let _bits = budget.try_reserve(Half::Skeleton, Bytes(words as u64 * 8))?;
    let mut taken = vec![0u64; words];
    // Candidates in A order are mostly in B order too; gathering a word's
    // bits in a register spares a load and a store per candidate.
    let (mut word, mut bits) = (0, 0u64);
    for p in &lines.pairs {
        let w = p.b as usize / 64;
        if w != word {
            taken[word] |= bits;
            (word, bits) = (w, 0);
        }
        bits |= 1 << (p.b % 64);
    }
    if let Some(last) = taken.get_mut(word) {
        *last |= bits;
    }
    let mut count_b = 0;
    zero_runs(&taken, b.len(), min, &mut |_| count_b += 1);
    if count_b == 0 {
        return Ok(());
    }
    let windows = {
        let _gaps = budget.try_reserve(
            Half::Skeleton,
            Bytes((count_a + count_b) as u64 * size_of::<Range<usize>>() as u64),
        )?;
        let (mut ga, mut gb) = (Vec::with_capacity(count_a), Vec::with_capacity(count_b));
        gaps_a(&mut |r| ga.push(r));
        zero_runs(&taken, b.len(), min, &mut |r| gb.push(r));
        Shingles { w: SHINGLE_W }.in_gaps((a, &ga), (b, &gb), budget)?
    };
    merge(lines, windows, &mut taken, budget)
}

/// Hands `f` each run of at least `min` indices below `n` that the
/// increasing indices in `cuts` leave between them, before the first or
/// after the last.
fn long_gaps(
    cuts: impl Iterator<Item = usize>,
    n: usize,
    min: usize,
    f: &mut dyn FnMut(Range<usize>),
) {
    let mut start = 0;
    for cut in cuts {
        if cut - start >= min {
            f(start..cut);
        }
        start = cut + 1;
    }
    if n.saturating_sub(start) >= min {
        f(start..n);
    }
}

/// [`long_gaps`] with the cuts as the set bits of `bits`. A word with
/// every bit set, as almost all are on data with an id column, costs one
/// comparison.
fn zero_runs(bits: &[u64], n: usize, min: usize, f: &mut dyn FnMut(Range<usize>)) {
    let mut start = 0;
    for (w, &word) in bits.iter().enumerate() {
        if word == 0 {
            continue;
        }
        let base = w * 64;
        let first = base + word.trailing_zeros() as usize;
        if first - start >= min {
            f(start..first);
        }
        if word == u64::MAX {
            start = base + 64;
            continue;
        }
        start = first + 1;
        let mut rest = word & (word - 1);
        while rest != 0 {
            let cut = base + rest.trailing_zeros() as usize;
            if cut - start >= min {
                f(start..cut);
            }
            start = cut + 1;
            rest &= rest - 1;
        }
    }
    if n.saturating_sub(start) >= min {
        f(start..n);
    }
}

/// Merges `extra` into `into` by A, in place. Every pair of `into` stays;
/// a pair of `extra` is dropped when its A sample is used by `into` or
/// its B sample is set in `taken`, the B samples `into` uses, which for
/// two sources over the same samples takes a 64-bit hash collision. On a
/// budget error `into` is left as it was.
///
/// Memory, from the skeleton half: `into` grows by the pairs kept.
fn merge(into: &mut Anchors, mut extra: Anchors, taken: &mut [u64], budget: &Budget) -> Result<()> {
    let mut next = into.pairs.iter().map(|p| p.a).peekable();
    extra.pairs.retain(|q| {
        while next.next_if(|&a| a < q.a).is_some() {}
        let (word, bit) = (q.b as usize / 64, 1u64 << (q.b % 64));
        let keep = next.peek() != Some(&q.a) && taken[word] & bit == 0;
        if keep {
            taken[word] |= bit;
        }
        keep
    });
    let k = extra.pairs.len();
    if k == 0 {
        return Ok(());
    }
    let bytes = Bytes(k as u64 * PAIR_BYTES);
    into.reservation.grow(bytes)?;
    if into.pairs.try_reserve_exact(k).is_err() {
        into.reservation.shrink(bytes);
        return Err(BudgetExceeded {
            half: Half::Skeleton,
            requested: Bytes(k as u64 * PAIR_BYTES),
            available: budget.available(Half::Skeleton),
        }
        .into());
    }
    let n = into.pairs.len();
    into.pairs.resize(n + k, Pair { a: 0, b: 0 });
    // From the back, so every pair moves once and nothing is overwritten
    // before it is read.
    let (mut i, mut j) = (n, k);
    for w in (0..n + k).rev() {
        if j == 0 {
            break;
        }
        if i > 0 && into.pairs[i - 1].a > extra.pairs[j - 1].a {
            into.pairs[w] = into.pairs[i - 1];
            i -= 1;
        } else {
            into.pairs[w] = extra.pairs[j - 1];
            j -= 1;
        }
    }
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

fn line_hashes(
    samples: &[Sample],
) -> (
    usize,
    impl Iterator<Item = (usize, u64)> + Clone + Sync + '_,
) {
    (samples.len(), samples.iter().map(|s| s.hash).enumerate())
}

/// Pairs of sample indices whose hash occurs exactly once on each side, as
/// [`UniqueLines`] does for line hashes. Each side is a count and an
/// iterator that yields that many `(sample index, hash)` items; the caller
/// keeps the indices below `MANY`.
///
/// With many samples the table is cut into one range of slots per thread,
/// each filled by its own thread from the items whose hash lands there,
/// so every thread walks both iterators. A range that fills up, which
/// only skewed hashes can do, sends the whole table back to one thread.
fn unique_hashes<I: Iterator<Item = (usize, u64)> + Clone + Sync>(
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

    let total = usize::try_from(slots).expect("slot count fits usize");
    let mut table = vec![VACANT_ENTRY; total];
    // Ranges at the full slot count's load, half full, have room to spare
    // for the noise in how many hashes land in each.
    let parts = if small.0 >= PARALLEL_SAMPLES && 2 * slots > 3 * n {
        crate::budget::MAX_THREADS.clamp(1, MAX_PARTS)
    } else {
        1
    };
    if !fill(&mut table, parts, &small.1, &big.1) {
        table.fill(VACANT_ENTRY);
        fill(&mut table, 1, &small.1, &big.1);
    }
    let len = total.div_ceil(parts);
    let mut counts = [0usize; MAX_PARTS];
    let unique = |e: &Entry| e.small < MANY && e.big < MANY;
    if parts == 1 {
        counts[0] = table.iter().filter(|e| unique(e)).count();
    } else {
        table
            .par_chunks(len)
            .zip(&mut counts[..parts])
            .for_each(|(range, count)| *count = range.iter().filter(|e| unique(e)).count());
    }
    let mut pairs = Vec::with_capacity(small.0);
    pairs.resize(counts.iter().sum(), Pair { a: 0, b: 0 });
    let pair = |e: &Entry| {
        if swapped {
            Pair {
                a: e.big,
                b: e.small,
            }
        } else {
            Pair {
                a: e.small,
                b: e.big,
            }
        }
    };
    let mut rest = &mut pairs[..];
    let mut ranges: [(&[Entry], &mut [Pair]); MAX_PARTS] =
        std::array::from_fn(|_| (&[][..], Default::default()));
    for (k, range) in table.chunks(len).enumerate() {
        let (out, tail) = rest.split_at_mut(counts[k]);
        ranges[k] = (range, out);
        rest = tail;
    }
    let gather = |(range, out): &mut (&[Entry], &mut [Pair])| {
        for (slot, e) in out.iter_mut().zip(range.iter().filter(|e| unique(e))) {
            *slot = pair(e);
        }
    };
    if parts == 1 {
        gather(&mut ranges[0]);
    } else {
        ranges[..parts].par_iter_mut().for_each(gather);
    }
    drop(table);
    if parts == 1 {
        pairs.sort_unstable_by_key(|p| p.a);
    } else {
        pairs.par_sort_unstable_by_key(|p| p.a);
    }
    let mut anchors = Anchors { pairs, reservation };
    // The table's bytes stay reserved until the trim, which briefly holds
    // the old and the new block.
    release_slack(&mut anchors.pairs, &mut anchors.reservation);
    anchors.reservation.shrink(Bytes(table_bytes));
    Ok(anchors)
}

/// Fewest samples on the smaller side for the table to be filled by
/// several threads; below it a thread fills it faster than they start.
// Miri runs the parallel path on a small table; the threshold only
// decides when splitting pays off, not what the result is.
const PARALLEL_SAMPLES: usize = if cfg!(miri) { 1 << 10 } else { 1 << 16 };
/// Most ranges the table is cut into.
const MAX_PARTS: usize = 64;

const VACANT_ENTRY: Entry = Entry {
    hash: 0,
    small: VACANT,
    big: VACANT,
};

/// Counts the hashes of `small`, then of `big`, into `table` cut into
/// `parts` ranges of slots. `false` when a range filled up.
fn fill<I: Iterator<Item = (usize, u64)> + Clone + Sync>(
    table: &mut [Entry],
    parts: usize,
    small: &I,
    big: &I,
) -> bool {
    let total = table.len();
    let len = total.div_ceil(parts);
    let run = |(k, range): (usize, &mut [Entry])| {
        let mut t = Table {
            slots: range,
            start: k * len,
            total,
            used: 0,
        };
        let mine = |h: u64| parts == 1 || home(h, total) / len == k;
        for (i, hash) in small.clone() {
            if mine(hash) && !t.count_small(hash, index(i)) {
                return false;
            }
        }
        for (i, hash) in big.clone() {
            if mine(hash) {
                t.count_big(hash, index(i));
            }
        }
        true
    };
    if parts == 1 {
        run((0, table))
    } else {
        table.par_chunks_mut(len).enumerate().all(run)
    }
}

/// The slot a hash starts probing from, out of `total`: its high bits,
/// since it is already uniform (the low bits are the stratum's zeros).
fn home(hash: u64, total: usize) -> usize {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the product shifted by 64 is below total"
    )]
    let slot = ((u128::from(hash) * total as u128) >> 64) as usize;
    slot
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "select checked both sample counts are below MANY"
)]
pub(crate) fn index(i: usize) -> u32 {
    i as u32
}

/// Open addressing with linear probing over one range of a table's slots,
/// keyed by the line hash itself. Probes wrap within the range.
struct Table<'t> {
    slots: &'t mut [Entry],
    /// Index of `slots[0]` in the whole table.
    start: usize,
    /// Slots in the whole table.
    total: usize,
    /// Occupied slots.
    used: usize,
}

impl Table<'_> {
    /// The slot holding `hash`, or the vacant slot where it belongs. A
    /// vacant slot exists: the whole table's load cap guarantees it for a
    /// single range, and `count_small` stops a range from filling.
    fn find(&self, hash: u64) -> usize {
        let len = self.slots.len();
        let mut i = home(hash, self.total) - self.start;
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

    /// Counts a hash of the smaller side; `false`, counting nothing, when
    /// it would take the range's last vacant slot.
    fn count_small(&mut self, hash: u64, i: u32) -> bool {
        let slot = self.find(hash);
        let full = self.used + 1 == self.slots.len();
        let e = &mut self.slots[slot];
        if e.small == VACANT {
            if full {
                return false;
            }
            self.used += 1;
            *e = Entry {
                hash,
                small: i,
                big: VACANT,
            };
        } else {
            e.small = MANY;
        }
        true
    }

    fn count_big(&mut self, hash: u64, i: u32) {
        let slot = self.find(hash);
        let e = &mut self.slots[slot];
        if e.small != VACANT {
            e.big = if e.big == VACANT { i } else { MANY };
        }
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

    /// Pairs from a hash map, for checking the table.
    fn oracle(ha: &[u64], hb: &[u64]) -> Vec<Pair> {
        let count = |h: &[u64]| {
            let mut m = std::collections::HashMap::new();
            for (i, &x) in h.iter().enumerate() {
                m.entry(x).or_insert((0, i)).0 += 1;
            }
            m
        };
        let (ca, cb) = (count(ha), count(hb));
        let mut out: Vec<Pair> = ca
            .iter()
            .filter(|&(_, &(n, _))| n == 1)
            .filter_map(|(h, &(_, a))| match cb.get(h) {
                Some(&(1, b)) => Some(Pair {
                    a: u32::try_from(a).unwrap(),
                    b: u32::try_from(b).unwrap(),
                }),
                _ => None,
            })
            .collect();
        out.sort_unstable_by_key(|p| p.a);
        out
    }

    #[test]
    fn many_samples_split_the_table_with_the_same_result() {
        let mut x = 7u64;
        let mut rnd = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        // Uniform hashes with repeats on each side; then a quarter of them
        // crowded into the first hundredth of the slots, which overfills
        // the first of eight ranges and sends the table to one thread.
        let spread = |x: u64| x.wrapping_mul(0x9E37_79B9_7F4A_7C15);
        // Miri: 2 000 and 2 000 hashes (the smaller side, 1 500, is above
        // Miri's PARALLEL_SAMPLES of 1 024); native: 200 000 and 120 000.
        let (n_uniform, distinct, n_crowded) = if cfg!(miri) {
            (2_000, 1_500, 2_000u64)
        } else {
            (200_000, 150_000, 120_000u64)
        };
        let uniform: Vec<u64> = (0..n_uniform).map(|_| spread(rnd() % distinct)).collect();
        let crowded: Vec<u64> = (0..n_crowded)
            .map(|i| if i % 4 == 0 { i << 40 } else { spread(i) })
            .collect();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(8)
            .build()
            .unwrap();
        for ha in [uniform, crowded] {
            let mut hb = ha.clone();
            hb.rotate_left(1000);
            hb.truncate(hb.len() * 3 / 4);
            let budget = Budget::new(Bytes::mib(64));
            let (sa, sb) = (hashed(&ha), hashed(&hb));
            let got = pool
                .install(|| unique_hashes(line_hashes(&sa), line_hashes(&sb), &budget))
                .unwrap();
            assert_eq!(got.pairs(), oracle(&ha, &hb));
        }
    }

    fn hashed(h: &[u64]) -> Vec<Sample> {
        h.iter()
            .map(|&hash| Sample {
                hash,
                line: LineNo(0),
                offset: ByteOffset(0),
            })
            .collect()
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
        // Miri: every 128th budget (64 of 1024); the table sizes sit well
        // inside that step, so both outcomes still appear.
        let step = if cfg!(miri) { 128 } else { 8 };
        for bytes in (0..8192).step_by(step) {
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
        let mut taken = taken(&into, 12);
        assert!(merge(&mut into, extra, &mut taken, &budget).is_err());
        assert_eq!(into.pairs(), [Pair { a: 1, b: 1 }, Pair { a: 3, b: 3 }]);
    }

    #[test]
    fn a_merged_pair_keeps_the_unique_line_on_a_clash() {
        let budget = Budget::new(Bytes::mib(1));
        let anchors = |pairs: &[(u32, u32)]| Anchors {
            pairs: pairs.iter().map(|&(a, b)| Pair { a, b }).collect(),
            reservation: budget.try_reserve(Half::Skeleton, Bytes(0)).unwrap(),
        };
        let mut into = anchors(&[(1, 1), (3, 3), (6, 6)]);
        let extra = anchors(&[(0, 0), (1, 1), (2, 3), (3, 5), (4, 4), (5, 1), (7, 2)]);
        let mut taken = taken(&into, 8);
        merge(&mut into, extra, &mut taken, &budget).unwrap();
        let got: Vec<(u32, u32)> = into.pairs().iter().map(|p| (p.a, p.b)).collect();
        assert_eq!(got, [(0, 0), (1, 1), (3, 3), (4, 4), (6, 6), (7, 2)]);
    }

    fn taken(anchors: &Anchors, n_b: usize) -> Vec<u64> {
        let mut bits = vec![0; n_b.div_ceil(64)];
        for p in anchors.pairs() {
            bits[p.b as usize / 64] |= 1 << (p.b % 64);
        }
        bits
    }

    #[test]
    fn gaps_are_the_long_runs_between_cuts() {
        let gaps = |cuts: &[usize], n: usize, min: usize| {
            let mut out = Vec::new();
            long_gaps(cuts.iter().copied(), n, min, &mut |r| out.push(r));
            let mut bits = vec![0u64; n.div_ceil(64)];
            for &c in cuts {
                bits[c / 64] |= 1 << (c % 64);
            }
            let mut runs = Vec::new();
            zero_runs(&bits, n, min, &mut |r| runs.push(r));
            assert_eq!(out, runs, "{cuts:?}");
            out
        };
        assert_eq!(gaps(&[2, 3, 9], 14, 3), [4..9, 10..14]);
        assert_eq!(gaps(&[], 2, 3), []);
        assert_eq!(gaps(&[], 5, 3), vec![Range { start: 0, end: 5 }]);
        let full: Vec<usize> = (0..64).chain([100, 140]).chain(150..200).collect();
        assert_eq!(gaps(&full, 260, 30), [64..100, 101..140, 200..260]);
    }
}
