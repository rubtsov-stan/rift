//! Content-defined chunks of whole lines, for saved results that a later
//! run matches against a changed file.
//!
//! Line `i` starts a chunk when the high 32 bits of its xxh3 are below a
//! threshold `thr = round(2^32 / n)`, with `n = max(1, 0.75 * target /
//! mean line length)` taken from the old file and kept with its table, so
//! the same lines start chunks in the new file. The high bits keep chunk
//! starts independent of strata, which use the low bits. A start closer
//! than `target / 4` to the previous one is ignored, a chunk that reaches
//! `4 * target` is cut at the next line start, and a line longer than that
//! is a chunk of its own. A chunk records the xxh3-128 of its bytes, its
//! byte length and its line count.
//!
//! The default target is 16 KiB: on the probe's 10 GiB pairs it had the
//! smallest changed share for scattered edits of the sizes tried (1.7% at
//! 0.01% of lines changed, against 6.1% at 64 KiB) and the same share for
//! appends. A table costs 32 bytes per chunk, 0.2% of the file.
//!
//! Memory: [`table`] takes its read buffer and the table from the half the
//! caller names, the table growing with a reservation taken first.

use xxhash_rust::xxh3::{Xxh3, xxh3_64};

use memchr::memchr_iter;

use crate::budget::{Budget, Half, Reservation};
use crate::error::Result;
use crate::source::Source;
use crate::types::Bytes;

/// Target chunk size when none is given.
pub const DEFAULT_TARGET: Bytes = Bytes(16 << 10);
/// Least and greatest target accepted.
pub const TARGETS: std::ops::RangeInclusive<u64> = (16 << 10)..=(32 << 10);
/// Read buffer of [`table`].
const READ_BUF: usize = 256 << 10;
const CHUNK_BYTES: u64 = size_of::<Chunk>() as u64;

/// How one side is cut, fixed when its first table is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    /// Target chunk size.
    pub target: Bytes,
    /// A line starts a chunk when the high 32 bits of its hash are below
    /// this, at most `2^32`.
    pub thr: u64,
}

impl Params {
    /// Parameters for a file of `bytes` bytes in `lines` lines, with
    /// `target` clamped to [`TARGETS`].
    #[must_use]
    #[allow(
        clippy::cast_precision_loss,
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a rounded threshold in 1..=2^32"
    )]
    pub fn new(target: Bytes, bytes: u64, lines: u64) -> Self {
        let target = Bytes(target.0.clamp(*TARGETS.start(), *TARGETS.end()));
        let mean = if lines == 0 {
            target.0 as f64
        } else {
            (bytes as f64 / lines as f64).max(1.0)
        };
        let n = (0.75 * target.0 as f64 / mean).max(1.0);
        let thr = ((1u64 << 32) as f64 / n).round() as u64;
        Self {
            target,
            thr: thr.clamp(1, 1 << 32),
        }
    }

    fn min(self) -> u64 {
        self.target.0 / 4
    }

    fn max(self) -> u64 {
        4 * self.target.0
    }

    /// Whether a line of `len` bytes with `hash` starts a chunk after
    /// `open` bytes of the current one.
    fn starts(self, open: u64, len: u64, hash: u64) -> bool {
        open > 0
            && (open >= self.max()
                || len > self.max()
                || (open >= self.min() && (hash >> 32) < self.thr))
    }
}

/// One chunk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Chunk {
    /// xxh3-128 of the chunk's bytes.
    pub hash: u128,
    /// Byte length.
    pub bytes: u64,
    /// Lines.
    pub lines: u64,
}

/// Cuts a stream of lines into chunks.
pub struct Chunker {
    params: Params,
    cur: Xxh3,
    bytes: u64,
    lines: u64,
    /// For a line arriving in pieces: the chunk as it was before the line,
    /// and the line alone, in case the line turns out to start a chunk.
    long: Option<(Chunk, Xxh3, u64)>,
}

impl std::fmt::Debug for Chunker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chunker")
            .field("params", &self.params)
            .field("bytes", &self.bytes)
            .field("lines", &self.lines)
            .finish_non_exhaustive()
    }
}

impl Chunker {
    /// A chunker at the start of a file.
    #[must_use]
    pub fn new(params: Params) -> Self {
        Self {
            params,
            cur: Xxh3::new(),
            bytes: 0,
            lines: 0,
            long: None,
        }
    }

    /// A whole line, terminator included; returns the chunk it closed.
    pub fn line(&mut self, line: &[u8]) -> Option<Chunk> {
        let len = line.len() as u64;
        let done = self
            .params
            .starts(self.bytes, len, xxh3_64(line))
            .then(|| self.take());
        self.cur.update(line);
        self.bytes += len;
        self.lines += 1;
        done
    }

    /// The next piece of a line too long to arrive whole; [`end`] gives
    /// the last one.
    ///
    /// [`end`]: Chunker::end
    pub fn piece(&mut self, piece: &[u8]) {
        let (_, fresh, len) = self.long.get_or_insert_with(|| {
            let before = Chunk {
                hash: self.cur.digest128(),
                bytes: self.bytes,
                lines: self.lines,
            };
            (before, Xxh3::new(), 0)
        });
        fresh.update(piece);
        *len += piece.len() as u64;
        self.cur.update(piece);
    }

    /// The last piece of a long line and the line's xxh3; returns the
    /// chunk it closed.
    pub fn end(&mut self, tail: &[u8], hash: u64) -> Option<Chunk> {
        let Some((before, mut fresh, len)) = self.long.take() else {
            return self.line(tail);
        };
        let len = len + tail.len() as u64;
        fresh.update(tail);
        self.cur.update(tail);
        if self.params.starts(before.bytes, len, hash) {
            self.cur = fresh;
            self.bytes = len;
            self.lines = 1;
            Some(before)
        } else {
            self.bytes += len;
            self.lines += 1;
            None
        }
    }

    /// The last chunk, if any.
    #[must_use]
    pub fn finish(mut self) -> Option<Chunk> {
        (self.bytes > 0).then(|| self.take())
    }

    fn take(&mut self) -> Chunk {
        let c = Chunk {
            hash: self.cur.digest128(),
            bytes: self.bytes,
            lines: self.lines,
        };
        self.cur.reset();
        self.bytes = 0;
        self.lines = 0;
        c
    }
}

/// The chunks of one file and how it was cut. Dropping it releases the
/// reservation.
#[derive(Debug)]
pub struct ChunkTable {
    /// How the file was cut.
    pub params: Params,
    chunks: Vec<Chunk>,
    _reservation: Reservation,
}

impl ChunkTable {
    /// The chunks in file order.
    #[must_use]
    pub fn chunks(&self) -> &[Chunk] {
        &self.chunks
    }

    /// The table as the bytes of a `CHUNKS_CDC_A` or `CHUNKS_CDC_B`
    /// section of a saved result: the target, the threshold and the chunk
    /// count as `uvar`, then per chunk its xxh3-128 as 16 bytes little
    /// endian, its byte length and its line count as `uvar`. Held in
    /// `half`, at most 36 bytes per chunk.
    ///
    /// # Errors
    ///
    /// [`Error::Budget`](crate::Error::Budget) when `half` cannot hold it.
    pub fn encode(&self, budget: &Budget, half: Half) -> Result<(Vec<u8>, Reservation)> {
        let most = 30 + 36 * self.chunks.len();
        let held = budget.try_reserve(half, Bytes(most as u64))?;
        let mut out = Vec::new();
        out.try_reserve_exact(most).map_err(|_| oom())?;
        let put = |v: u64, out: &mut Vec<u8>| {
            let mut b = [0u8; 10];
            let n = crate::saved::uvar(v, &mut b);
            out.extend_from_slice(&b[..n]);
        };
        put(self.params.target.0, &mut out);
        put(self.params.thr, &mut out);
        put(self.chunks.len() as u64, &mut out);
        for c in &self.chunks {
            out.extend_from_slice(&c.hash.to_le_bytes());
            put(c.bytes, &mut out);
            put(c.lines, &mut out);
        }
        Ok((out, held))
    }

    /// A table from the bytes [`encode`](Self::encode) wrote, held in
    /// `half`.
    ///
    /// # Errors
    ///
    /// [`Error::Saved`](crate::Error::Saved) when the bytes are not such a
    /// table; [`Error::Budget`](crate::Error::Budget) when it does not fit.
    pub fn decode(bytes: &[u8], budget: &Budget, half: Half) -> Result<Self> {
        let bad = || crate::saved::corrupt("content-defined chunk table");
        let mut c = crate::saved::Cursor::new(bytes);
        let (target, thr, count) = (c.uvar()?, c.uvar()?, c.uvar()?);
        // A chunk takes at least 18 bytes, which bounds a forged count.
        if !TARGETS.contains(&target)
            || thr == 0
            || thr > 1 << 32
            || count > bytes.len() as u64 / 18
        {
            return Err(bad());
        }
        let count = usize::try_from(count).map_err(|_| bad())?;
        let held = budget.try_reserve(half, Bytes(count as u64 * CHUNK_BYTES))?;
        let mut chunks = Vec::new();
        chunks.try_reserve_exact(count).map_err(|_| oom())?;
        for _ in 0..count {
            let hash = u128::from_le_bytes(c.take(16)?.try_into().map_err(|_| bad())?);
            chunks.push(Chunk {
                hash,
                bytes: c.uvar()?,
                lines: c.uvar()?,
            });
        }
        if c.left() != 0 {
            return Err(bad());
        }
        Ok(Self {
            params: Params {
                target: Bytes(target),
                thr,
            },
            chunks,
            _reservation: held,
        })
    }
}

/// Reads `source` once and cuts it with `params`; the table and a read
/// buffer come from `half`.
///
/// # Errors
///
/// [`Error::Budget`](crate::Error::Budget) when `half` cannot hold them,
/// [`Error::Io`](crate::Error::Io) from the source.
pub fn table<S: Source + ?Sized>(
    source: &S,
    params: Params,
    budget: &Budget,
    half: Half,
) -> Result<ChunkTable> {
    let _buf_held = budget.try_reserve(half, Bytes(READ_BUF as u64))?;
    let mut held = budget.try_reserve(half, Bytes(0))?;
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut push = |c: Chunk, chunks: &mut Vec<Chunk>| -> Result<()> {
        if chunks.len() == chunks.capacity() {
            let more = chunks.capacity().max(64);
            held.grow(Bytes(more as u64 * CHUNK_BYTES))?;
            chunks.try_reserve_exact(more).map_err(|_| oom())?;
        }
        chunks.push(c);
        Ok(())
    };
    let mut chunker = Chunker::new(params);
    let mut long = Xxh3::new();
    let mut in_long = false;
    let mut buf = vec![0u8; READ_BUF];
    let (mut pos, mut filled) = (0u64, 0usize);
    loop {
        let n = crate::index::read_retrying(
            source,
            &mut buf[filled..],
            crate::ByteOffset(pos + filled as u64),
        )?;
        let end = filled + n;
        if n == 0 {
            if filled > 0 || in_long {
                let tail = &buf[..filled];
                let c = if in_long {
                    long.update(tail);
                    chunker.end(tail, long.digest())
                } else {
                    chunker.line(tail)
                };
                if let Some(c) = c {
                    push(c, &mut chunks)?;
                }
            }
            break;
        }
        let mut start = 0;
        for nl in memchr_iter(b'\n', &buf[filled..end]).map(|k| k + filled) {
            let line = &buf[start..=nl];
            let c = if in_long {
                in_long = false;
                long.update(line);
                chunker.end(line, long.digest())
            } else {
                chunker.line(line)
            };
            if let Some(c) = c {
                push(c, &mut chunks)?;
            }
            start = nl + 1;
        }
        if start == 0 && end == buf.len() {
            if !in_long {
                in_long = true;
                long.reset();
            }
            long.update(&buf[..end]);
            chunker.piece(&buf[..end]);
            pos += end as u64;
            filled = 0;
        } else {
            buf.copy_within(start..end, 0);
            filled = end - start;
            pos += start as u64;
        }
    }
    if let Some(c) = chunker.finish() {
        push(c, &mut chunks)?;
    }
    drop(push);
    let spare = chunks.capacity() - chunks.len();
    chunks.shrink_to_fit();
    held.shrink(Bytes(spare as u64 * CHUNK_BYTES));
    Ok(ChunkTable {
        params,
        chunks,
        _reservation: held,
    })
}

/// Old chunks searched ahead of the cursor for a new chunk that does not
/// match the next one.
const LOOKAHEAD: usize = 64;
/// Every this many old chunks one goes into the sparse table.
const SPARSE: usize = 64;

/// A stretch of chunks found in both tables in the same order: `bytes`
/// bytes and `lines` lines of the old file from `old` are the same chunks
/// in the new file from `new`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    /// Byte offset in the old file.
    pub old: u64,
    /// Byte offset in the new file.
    pub new: u64,
    /// Bytes in the run.
    pub bytes: u64,
}

/// Matches `new` against `old` in order, in one pass over each: a new
/// chunk equal to the next old one extends the run; otherwise it is looked
/// for among the next 64 old chunks, then among every
/// 64th old chunk further on, and an old chunk found there skips
/// the ones before it. A new chunk found nowhere is changed. Matches never
/// go back, so the runs increase in both files. Chunks match on hash and
/// length; the caller compares bytes before it relies on a run.
///
/// Memory, from `half`: 24 bytes per sparse entry and per run.
///
/// # Errors
///
/// [`Error::Budget`](crate::Error::Budget) when `half` cannot hold them.
pub fn match_runs(old: &[Chunk], new: &[Chunk], budget: &Budget, half: Half) -> Result<Runs> {
    let sparse_n = old.len().div_ceil(SPARSE);
    let mut held = budget.try_reserve(half, Bytes(sparse_n as u64 * 24))?;
    // Sorted by hash, then index, for a binary search.
    let mut sparse: Vec<(u128, usize)> = (0..old.len())
        .step_by(SPARSE)
        .map(|i| (old[i].hash, i))
        .collect();
    sparse.sort_unstable();
    let mut runs: Vec<Run> = Vec::new();
    let (mut i, mut old_at) = (0usize, 0u64);
    let mut new_at = 0u64;
    let same = |x: &Chunk, y: &Chunk| x.hash == y.hash && x.bytes == y.bytes;
    for c in new {
        let found = if old.get(i).is_some_and(|o| same(o, c)) {
            Some(i)
        } else {
            let ahead = old.iter().enumerate().skip(i + 1).take(LOOKAHEAD);
            ahead
                .clone()
                .find(|(_, o)| same(o, c))
                .map(|(k, _)| k)
                .or_else(|| {
                    let from = sparse.partition_point(|&(h, k)| (h, k) < (c.hash, i));
                    sparse[from..]
                        .iter()
                        .take_while(|&&(h, _)| h == c.hash)
                        .find(|&&(_, k)| same(&old[k], c))
                        .map(|&(_, k)| k)
                })
        };
        if let Some(k) = found {
            old_at += old[i..k].iter().map(|o| o.bytes).sum::<u64>();
            i = k;
            match runs.last_mut() {
                Some(r) if r.old + r.bytes == old_at && r.new + r.bytes == new_at => {
                    r.bytes += c.bytes;
                }
                _ => {
                    if runs.len() == runs.capacity() {
                        let more = runs.capacity().max(16);
                        held.grow(Bytes(more as u64 * 24))?;
                        runs.try_reserve_exact(more).map_err(|_| oom())?;
                    }
                    runs.push(Run {
                        old: old_at,
                        new: new_at,
                        bytes: c.bytes,
                    });
                }
            }
            old_at += old[i].bytes;
            i += 1;
        }
        new_at += c.bytes;
    }
    drop(sparse);
    held.shrink(Bytes(sparse_n as u64 * 24));
    Ok(Runs { runs, _held: held })
}

/// The runs [`match_runs`] found, in increasing order in both files.
/// Dropping it releases the reservation.
#[derive(Debug)]
pub struct Runs {
    runs: Vec<Run>,
    _held: Reservation,
}

impl Runs {
    /// The runs.
    #[must_use]
    pub fn runs(&self) -> &[Run] {
        &self.runs
    }
}

fn oom() -> crate::error::Error {
    std::io::Error::from(std::io::ErrorKind::OutOfMemory).into()
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss, reason = "a ratio of small sizes")]
mod tests {
    use super::*;

    fn cut(data: &[u8], params: Params) -> Vec<Chunk> {
        table(data, params, &Budget::new(Bytes::mib(8)), Half::Skeleton)
            .unwrap()
            .chunks()
            .to_vec()
    }

    fn rows(n: u64) -> Vec<u8> {
        (0..n)
            .flat_map(|i| format!("row {i} payload {}\n", i * 7919 % 1013).into_bytes())
            .collect()
    }

    #[test]
    fn chunks_tile_the_file_and_hash_their_bytes() {
        let data = rows(20_000);
        let p = Params::new(DEFAULT_TARGET, data.len() as u64, 20_000);
        let chunks = cut(&data, p);
        let mut at = 0usize;
        let mut lines = 0;
        for c in &chunks {
            let end = at + usize::try_from(c.bytes).unwrap();
            assert_eq!(c.hash, xxhash_rust::xxh3::xxh3_128(&data[at..end]));
            assert!(c.bytes <= p.max() + 64, "{c:?}");
            lines += c.lines;
            at = end;
        }
        assert_eq!((at, lines), (data.len(), 20_000));
        let mean = data.len() as f64 / chunks.len() as f64;
        assert!(
            (0.8..1.3).contains(&(mean / p.target.0 as f64)),
            "mean chunk {mean}"
        );
    }

    #[test]
    fn an_insertion_dirties_one_run_and_an_append_only_the_end() {
        let data = rows(20_000);
        let p = Params::new(DEFAULT_TARGET, data.len() as u64, 20_000);
        let old = cut(&data, p);
        let mut ins = data.clone();
        let mid = data.len() / 2;
        let mid = mid + data[mid..].iter().position(|&c| c == b'\n').unwrap() + 1;
        ins.splice(mid..mid, b"an inserted line\n".iter().copied());
        let new = cut(&ins, p);
        let same_head = old.iter().zip(&new).take_while(|(x, y)| x == y).count();
        let same_tail = old
            .iter()
            .rev()
            .zip(new.iter().rev())
            .take_while(|(x, y)| x == y)
            .count();
        let dirty: u64 = new[same_head..new.len() - same_tail]
            .iter()
            .map(|c| c.bytes)
            .sum();
        assert!(dirty > 0 && dirty <= 8 * p.target.0 + 17, "dirty {dirty}");

        let mut app = data.clone();
        app.extend_from_slice(&rows(500));
        let new = cut(&app, p);
        let kept = old.iter().zip(&new).take_while(|(x, y)| x == y).count();
        assert!(kept + 1 >= old.len(), "{kept} of {}", old.len());
    }

    #[test]
    fn a_long_line_cuts_as_if_it_came_whole() {
        let mut data = rows(3000);
        let at = data.len() / 3;
        let at = at + data[at..].iter().position(|&c| c == b'\n').unwrap() + 1;
        data.splice(at..at, std::iter::repeat_n(b'x', 600_000).chain(*b"\n"));
        let p = Params::new(DEFAULT_TARGET, data.len() as u64, 3001);
        let streamed = cut(&data, p);
        let mut whole = Chunker::new(p);
        let mut expect = Vec::new();
        for line in data.split_inclusive(|&c| c == b'\n') {
            expect.extend(whole.line(line));
        }
        expect.extend(whole.finish());
        assert_eq!(streamed, expect);
        assert!(streamed.iter().any(|c| c.lines == 1 && c.bytes == 600_001));
    }

    #[test]
    fn a_table_survives_a_saved_section() {
        let data = rows(5_000);
        let budget = Budget::new(Bytes::mib(8));
        let p = Params::new(DEFAULT_TARGET, data.len() as u64, 5_000);
        let t = table(data.as_slice(), p, &budget, Half::Skeleton).unwrap();
        let (bytes, _held) = t.encode(&budget, Half::Refine).unwrap();
        let back = ChunkTable::decode(&bytes, &budget, Half::Refine).unwrap();
        assert_eq!((back.params, back.chunks()), (t.params, t.chunks()));
        assert!(ChunkTable::decode(&bytes[..bytes.len() - 1], &budget, Half::Refine).is_err());
    }

    #[test]
    fn the_threshold_follows_the_mean_line() {
        assert_eq!(Params::new(Bytes(16 << 10), 1 << 20, 1 << 20).thr, 349_525);
        assert_eq!(Params::new(Bytes(16 << 10), 1 << 30, 1).thr, 1 << 32);
        assert_eq!(Params::new(Bytes(1), 100, 1).target, Bytes(16 << 10));
    }
}
