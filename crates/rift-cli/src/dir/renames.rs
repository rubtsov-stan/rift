//! Renames by content: a file only in A and a file only in B with the same
//! bytes are reported as one renamed file.
//!
//! The first walk counts, per bucket of file sizes, the one-sided files of
//! each tree; the second keeps the one-sided files whose size may occur in
//! the other tree, with a hash of their bytes, in a table per tree, each
//! with half the room, so a full table still holds files of both trees.
//! The tables are sorted and the files of one size and hash are paired
//! after a byte compare. A hash match alone is never a rename.
//!
//! Several files with the same content: removed and added files pair in
//! byte order of their paths, the first removed with the first added, the
//! second with the second; the rest stay only in their tree. The order
//! depends on the paths alone.

use std::cmp::Ordering;

use rift_core::{Budget, BudgetExceeded, Reservation};

use super::take;
use super::walk::Side;

/// Size buckets of [`Counts`]: one byte each, 64 KiB.
// A fixed bucket count: with far more one-sided sizes than this,
// most buckets fill on both sides and nearly every one-sided file is
// hashed. Size it from `--mem` if that costs measurable time.
pub const BUCKETS: usize = 1 << 16;

/// A file that tries this many files of its size and hash in vain stops
/// looking, and the table is marked approximate.
const MAX_MISSES: usize = 8;

/// Per bucket of sizes, the one-sided files of each tree that fall in it,
/// as two bit counters that stop at 3. A bucket at 0 on one side means no
/// file of that side has a size in it; at 1 on both sides, each side has
/// exactly one such file, so the two need no hash, only a compare.
pub struct Counts {
    buckets: Vec<u8>,
    _held: Reservation,
}

impl Counts {
    pub fn new(budget: &Budget) -> Result<Self, BudgetExceeded> {
        let held = take(budget, BUCKETS as u64)?;
        Ok(Self {
            buckets: vec![0; BUCKETS],
            _held: held,
        })
    }

    fn slot(size: u64) -> usize {
        // Fibonacci hashing: the top 16 bits of the product.
        (size.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 48) as usize
    }

    pub fn add(&mut self, side: Side, len: u64) {
        let shift = 2 * side as u8;
        let b = &mut self.buckets[Self::slot(len)];
        if (*b >> shift) & 3 < 3 {
            *b += 1 << shift;
        }
    }

    /// The counters of `size`'s bucket, A then B.
    pub fn get(&self, size: u64) -> (u8, u8) {
        let b = self.buckets[Self::slot(size)];
        (b & 3, b >> 2)
    }

    /// Whether some bucket has files of both trees.
    pub fn shared(&self) -> bool {
        self.buckets.iter().any(|&b| b & 3 != 0 && b >> 2 != 0)
    }
}

/// A candidate: a one-sided regular file, its size, the hash of its bytes
/// (0 when its bucket holds one file per tree) and its relative path, at
/// `off` in its table's path bytes. After [`pair`], `peer_*` is the path of
/// the file in the other table.
#[derive(Debug, Clone, Copy)]
pub struct Cand {
    size: u64,
    hash: u64,
    off: u64,
    peer_off: u64,
    len: u32,
    peer_len: u32,
}

const _: () = assert!(size_of::<Cand>() == 40);

const UNPAIRED: u32 = u32::MAX;

impl Cand {
    pub fn new(size: u64, hash: u64) -> Self {
        Self {
            size,
            hash,
            off: 0,
            peer_off: 0,
            len: 0,
            peer_len: UNPAIRED,
        }
    }

    fn key(&self) -> (u64, u64) {
        (self.size, self.hash)
    }
}

/// The candidates of one tree and their paths, held in the walk's share up
/// to `limit` bytes. Both vectors grow by doubling into a new allocation,
/// and the old one is counted until the copy is done.
pub struct Table<'b> {
    budget: &'b Budget,
    limit: u64,
    cands: Vec<Cand>,
    paths: Vec<u8>,
    held: [Option<Reservation>; 2],
    /// A candidate did not fit: renames past it are not looked for.
    pub full: bool,
}

impl<'b> Table<'b> {
    pub fn new(budget: &'b Budget, limit: u64) -> Self {
        Self {
            budget,
            limit,
            cands: Vec::new(),
            paths: Vec::new(),
            held: [None, None],
            full: false,
        }
    }

    fn held(&self) -> u64 {
        self.held.iter().flatten().map(|r| r.bytes().0).sum()
    }

    /// Adds `c` with path `path`; false, and the table full from then on,
    /// when it does not fit.
    pub fn push(&mut self, mut c: Cand, path: &[u8]) -> bool {
        let held = self.held();
        let fits = !self.full
            && grow(
                self.budget,
                self.limit,
                held,
                &mut self.cands,
                &mut self.held[0],
                1,
            );
        let held = self.held();
        if !fits
            || !grow(
                self.budget,
                self.limit,
                held,
                &mut self.paths,
                &mut self.held[1],
                path.len(),
            )
        {
            self.full = true;
            return false;
        }
        c.off = self.paths.len() as u64;
        c.len = u32::try_from(path.len()).unwrap_or(u32::MAX);
        self.paths.extend_from_slice(path);
        self.cands.push(c);
        true
    }

    fn path(&self, c: &Cand) -> &[u8] {
        path_of(&self.paths, c.off, c.len)
    }

    /// The path in `other` that the file at `rel` was paired with.
    pub fn peer<'o>(&self, rel: &[u8], other: &'o Table<'_>) -> Option<&'o [u8]> {
        let i = self
            .cands
            .binary_search_by(|c| path_cmp(self.path(c), rel))
            .ok()?;
        let c = &self.cands[i];
        Some(path_of(&other.paths, c.peer_off, c.peer_len))
    }
}

/// Makes room for `extra` more items in `v`, whose reservation is `held`,
/// within `limit` bytes for the table, `table` of them held now.
fn grow<T: Copy>(
    budget: &Budget,
    limit: u64,
    table: u64,
    v: &mut Vec<T>,
    held: &mut Option<Reservation>,
    extra: usize,
) -> bool {
    let need = v.len() + extra;
    if need <= v.capacity() {
        return true;
    }
    let cap = need.max(v.capacity() * 2).max(64);
    let bytes = (cap * size_of::<T>()) as u64;
    if table.saturating_add(bytes) > limit {
        return false;
    }
    let Ok(r) = take(budget, bytes) else {
        return false;
    };
    let mut bigger = Vec::with_capacity(cap);
    bigger.extend_from_slice(v);
    *v = bigger;
    *held = Some(r);
    true
}

fn path_of(paths: &[u8], off: u64, len: u32) -> &[u8] {
    let off = usize::try_from(off).unwrap_or(usize::MAX);
    &paths[off..off + len as usize]
}

/// Orders relative paths as the walk visits them: by component, each in
/// byte order, so `a/x` comes before `a.txt`. Names hold neither `/` nor
/// NUL, so mapping `/` to 0 gives that order.
pub fn path_cmp(x: &[u8], y: &[u8]) -> Ordering {
    let key = |c: &u8| if *c == b'/' { 0 } else { *c };
    x.iter().map(key).cmp(y.iter().map(key))
}

/// Pairs the candidates of `a` and `b`: within one size and hash, each
/// file of A in path order takes the first file of B in path order, not
/// yet taken, for which `same` holds. Keeps only the paired candidates,
/// sorted by path for [`Table::peer`]. Returns true when a file stopped
/// looking after [`MAX_MISSES`] failed compares.
pub fn pair<'b>(
    a: &mut Table<'b>,
    b: &mut Table<'b>,
    mut same: impl FnMut(&[u8], &[u8]) -> bool,
) -> bool {
    for t in [&mut *a, &mut *b] {
        let paths = &t.paths;
        t.cands.sort_unstable_by(|x, y| {
            x.key()
                .cmp(&y.key())
                .then_with(|| path_cmp(path_of(paths, x.off, x.len), path_of(paths, y.off, y.len)))
        });
    }
    let mut cut = false;
    let (mut i, mut j) = (0, 0);
    while i < a.cands.len() && j < b.cands.len() {
        let key = a.cands[i].key();
        match key.cmp(&b.cands[j].key()) {
            Ordering::Less => i += 1,
            Ordering::Greater => j += 1,
            Ordering::Equal => {
                let end_a = i + a.cands[i..].iter().take_while(|c| c.key() == key).count();
                let end_b = j + b.cands[j..].iter().take_while(|c| c.key() == key).count();
                for x in i..end_a {
                    let mut misses = 0;
                    for y in j..end_b {
                        if b.cands[y].peer_len != UNPAIRED {
                            continue;
                        }
                        if same(a.path(&a.cands[x]), b.path(&b.cands[y])) {
                            let (cx, cy) = (a.cands[x], b.cands[y]);
                            (a.cands[x].peer_off, a.cands[x].peer_len) = (cy.off, cy.len);
                            (b.cands[y].peer_off, b.cands[y].peer_len) = (cx.off, cx.len);
                            break;
                        }
                        misses += 1;
                        if misses == MAX_MISSES {
                            cut = true;
                            break;
                        }
                    }
                }
                (i, j) = (end_a, end_b);
            }
        }
    }
    for t in [a, b] {
        t.cands.retain(|c| c.peer_len != UNPAIRED);
        let paths = &t.paths;
        t.cands.sort_unstable_by(|x, y| {
            path_cmp(path_of(paths, x.off, x.len), path_of(paths, y.off, y.len))
        });
    }
    cut
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_core::Bytes;

    /// Tables of A and B from (side, path, hash), every file of size 4.
    fn tables<'b>(budget: &'b Budget, files: &[(Side, &str, u64)]) -> [Table<'b>; 2] {
        let mut t = [Table::new(budget, 1 << 20), Table::new(budget, 1 << 20)];
        for &(side, path, hash) in files {
            assert!(t[side as usize].push(Cand::new(4, hash), path.as_bytes()));
        }
        t
    }

    fn pairs(
        t: &mut [Table<'_>; 2],
        same: impl FnMut(&[u8], &[u8]) -> bool,
    ) -> Vec<(String, String)> {
        let [a, b] = t;
        pair(a, b, same);
        a.cands
            .iter()
            .map(|c| {
                let peer = a.peer(a.path(c), b).unwrap();
                (
                    String::from_utf8(a.path(c).to_vec()).unwrap(),
                    String::from_utf8(peer.to_vec()).unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn a_hash_match_without_equal_bytes_is_no_rename() {
        let budget = Budget::new(Bytes::mib(16));
        let mut t = tables(&budget, &[(Side::A, "x", 7), (Side::B, "y", 7)]);
        assert_eq!(pairs(&mut t, |_, _| false), Vec::<(String, String)>::new());
        assert_eq!(t[1].peer(b"y", &t[0]), None);
    }

    #[test]
    fn duplicates_pair_in_path_order_whatever_the_input_order() {
        let budget = Budget::new(Bytes::mib(16));
        let files = [
            (Side::B, "e/y3", 1),
            (Side::A, "d/x2", 1),
            (Side::B, "e/y1", 1),
            (Side::A, "d/x1", 1),
            (Side::B, "e/y2", 1),
        ];
        let want = vec![
            ("d/x1".into(), "e/y1".into()),
            ("d/x2".into(), "e/y2".into()),
        ];
        let mut t = tables(&budget, &files);
        assert_eq!(pairs(&mut t, |_, _| true), want);
        let mut reversed = files;
        reversed.reverse();
        let mut t = tables(&budget, &reversed);
        assert_eq!(pairs(&mut t, |_, _| true), want);
    }

    #[test]
    fn a_file_takes_the_first_equal_file_not_the_first_file() {
        let budget = Budget::new(Bytes::mib(16));
        let mut t = tables(
            &budget,
            &[(Side::A, "a", 1), (Side::B, "b1", 1), (Side::B, "b2", 1)],
        );
        assert_eq!(
            pairs(&mut t, |_, b| b == b"b2"),
            vec![("a".into(), "b2".into())]
        );
    }

    #[test]
    fn paths_order_by_component() {
        assert_eq!(path_cmp(b"a/x", b"a.txt"), Ordering::Less);
        assert_eq!(path_cmp(b"a", b"a/x"), Ordering::Less);
        assert_eq!(path_cmp(b"a-b", b"a/b"), Ordering::Greater);
    }

    #[test]
    fn a_full_table_says_so_and_takes_no_more() {
        let budget = Budget::new(Bytes::mib(16));
        let mut t = Table::new(&budget, 64 * 40 + 64);
        let mut n = 0;
        while t.push(Cand::new(1, 1), b"p") {
            n += 1;
        }
        assert!(t.full && n > 0);
        assert!(!t.push(Cand::new(1, 1), b""));
    }
}
