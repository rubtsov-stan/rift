//! A saved result as a [`DiffView`], with its text read from the original
//! files through chunk checks.

use std::io;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use xxhash_rust::xxh3::xxh3_64;

use super::{FLAG_COMPLETE, Map, SavedFile};
use crate::budget::{Budget, Half, Reservation};
use crate::edit::{Edit, Hunk};
use crate::error::{Error, Result};
use crate::index::{IndexedLines, LineIndex};
use crate::lines::LineAccess;
use crate::moves::MovedBlock;
use crate::segment::{Segment, SegmentStatus};
use crate::source::Source;
use crate::types::{ByteOffset, Bytes, LineNo};
use crate::view::{ApproxTally, DiffView, HunkState, Progress};

/// Read buffer of the front end's line readers, as for a live diff.
const FRONT_SCRATCH: Bytes = Bytes(16 << 10);
/// Decoded blocks the cache keeps at most, whatever their size.
const CACHE_SLOTS: usize = 16;

/// A [`Source`] whose every read is checked against the chunk hashes a
/// result holds for it.
///
/// A read loads the whole chunk it falls in into one buffer, hashes the
/// buffer, compares it with the table and serves the bytes from that same
/// buffer, so a writer cannot change the file between the check and the
/// read. The last chunk checked stays in the buffer for the next read.
/// After one mismatch every read fails: nothing from the file is served
/// again.
///
/// Memory: the chunk buffer and the table, from the Refine half.
#[derive(Debug)]
pub struct Verified<S> {
    inner: S,
    name: &'static str,
    len: u64,
    shift: u32,
    table: Vec<u64>,
    state: Mutex<(u64, Vec<u8>)>,
    failed: AtomicBool,
    _held: [Reservation; 2],
}

impl<S: Source> Verified<S> {
    /// Checks reads of `inner`, called `name` in errors, against `table`
    /// of chunks of `1 << shift` bytes, for a file of `len` bytes.
    ///
    /// # Errors
    ///
    /// [`Error::Budget`] when the Refine half cannot hold the buffer;
    /// [`Error::Saved`] when `inner` is not `len` bytes long.
    pub fn new(
        inner: S,
        name: &'static str,
        len: u64,
        shift: u32,
        (table, table_held): (Vec<u64>, Reservation),
        budget: &Budget,
    ) -> Result<Self> {
        if inner.len().0 != len {
            return Err(Error::Saved(format!(
                "{name} is {} bytes, the result describes {len}: the file changed",
                inner.len().0
            )));
        }
        let chunk = (1u64 << shift).min(len);
        let held = budget.try_reserve(Half::Refine, Bytes(chunk))?;
        Ok(Self {
            inner,
            name,
            len,
            shift,
            table,
            state: Mutex::new((u64::MAX, vec![0; chunk as usize])),
            failed: AtomicBool::new(false),
            _held: [table_held, held],
        })
    }

    /// Whether a chunk failed its check.
    #[must_use]
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }

    fn changed(&self) -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} changed since the result was saved", self.name),
        )
    }
}

impl<S: Source> Source for Verified<S> {
    fn len(&self) -> Bytes {
        Bytes(self.len)
    }

    fn read_at(&self, buf: &mut [u8], offset: ByteOffset) -> io::Result<usize> {
        if self.failed() {
            return Err(self.changed());
        }
        if offset.0 >= self.len || buf.is_empty() {
            return Ok(0);
        }
        let k = offset.0 >> self.shift;
        let start = k << self.shift;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let (loaded, chunk) = &mut *state;
        let n = usize::try_from((self.len - start).min(1 << self.shift)).expect("chunk fits usize");
        if *loaded != k {
            *loaded = u64::MAX;
            self.inner
                .read_exact_at(&mut chunk[..n], ByteOffset(start))?;
            let want = self
                .table
                .get(usize::try_from(k).expect("chunk number fits usize"));
            if want != Some(&xxh3_64(&chunk[..n])) {
                // Relaxed: the flag guards no memory; a reader that misses
                // it still fails its own check.
                self.failed.store(true, Ordering::Relaxed);
                return Err(self.changed());
            }
            *loaded = k;
        }
        let from = usize::try_from(offset.0 - start).expect("inside the chunk");
        let m = buf.len().min(n - from);
        buf[..m].copy_from_slice(&chunk[from..from + m]);
        Ok(m)
    }
}

type Lines<S> = IndexedLines<Verified<S>, LineIndex>;

/// A saved result opened for a front end: [`DiffView`] over the decoded
/// map, with line text from the original files through [`Verified`].
///
/// Memory. Skeleton half: the segment map at 56 bytes per segment (the
/// segment and its byte starts, as the live diff holds them), the
/// block index at 32 bytes per block, both line indexes at 16 bytes per
/// checkpoint. A result written under the same `--mem` fits: the live
/// diff held the map, its byte starts and both indexes in that half.
/// Refine half: the meta section, both chunk tables at 8 bytes per MiB,
/// two chunk buffers of 1 MiB, two line readers of 16 KiB, the moved
/// blocks, a buffer for the longest block, and a cache of decoded blocks:
/// at least the largest decoded block, up to four times that or 4 MiB,
/// within half of what the Refine half has free.
#[derive(Debug)]
pub struct SavedDiff<R, A, B> {
    file: SavedFile<R>,
    map: Map,
    lines_a: Lines<A>,
    lines_b: Lines<B>,
    moves: Vec<MovedBlock>,
    moves_flags: [bool; 3],
    cache: Mutex<Cache>,
    _held: [Reservation; 4],
}

/// A decoded block: its number, its scripts, their heap.
type Slot = (usize, Arc<[Option<Arc<Hunk>>]>, u64);

#[derive(Debug)]
struct Cache {
    buf: Vec<u8>,
    cap: u64,
    used: u64,
    /// Most recent first: block, its scripts, their heap.
    slots: Vec<Slot>,
    error: Option<String>,
}

impl<R: Source, A: Source, B: Source> SavedDiff<R, A, B> {
    /// Opens the result `file` over `a` and `b`, the files it describes.
    /// Decodes and checks the whole map; the scripts are decoded on
    /// demand.
    ///
    /// # Errors
    ///
    /// [`Error::Saved`] when the result is partial (it has no script for
    /// some differing segment), corrupt, or a file's length differs from
    /// the one saved; [`Error::Budget`] when the map or the buffers do not
    /// fit; [`Error::Io`] from any source.
    pub fn open(file: SavedFile<R>, a: A, b: B, budget: &Budget) -> Result<Self> {
        if file.flags() & FLAG_COMPLETE == 0 || file.meta().refined != file.meta().differing() {
            return Err(Error::Saved(
                "the result has no edit scripts for some changes (a --summary save); \
                 recompute it without --summary to open it"
                    .into(),
            ));
        }
        let (sizes, shift) = (
            [file.meta().files[0].size, file.meta().files[1].size],
            file.chunk_shift(),
        );
        let va = Verified::new(
            a,
            "a",
            sizes[0],
            shift,
            file.chunk_table(0, budget, Half::Refine)?,
            budget,
        )?;
        let vb = Verified::new(
            b,
            "b",
            sizes[1],
            shift,
            file.chunk_table(1, budget, Half::Refine)?,
            budget,
        )?;
        let map = file.map(budget)?;
        let ([ia, ib], indexes) = file.line_indexes(budget)?;
        let lines_a = IndexedLines::new(va, ia, budget, Half::Refine, FRONT_SCRATCH)?;
        let lines_b = IndexedLines::new(vb, ib, budget, Half::Refine, FRONT_SCRATCH)?;
        let (moves, moves_flags, moves_held) = file.moves(budget)?;
        let buf_held = budget.try_reserve(Half::Refine, Bytes(map.max_block))?;
        let free = budget.available(Half::Refine).0 / 2;
        let cap = (4 * map.max_decoded)
            .max(4 << 20)
            .min(free)
            .max(map.max_decoded);
        let cache_held = budget.try_reserve(Half::Refine, Bytes(cap))?;
        let cache = Cache {
            buf: vec![0; map.max_block as usize],
            cap,
            used: 0,
            slots: Vec::with_capacity(CACHE_SLOTS),
            error: None,
        };
        Ok(Self {
            file,
            map,
            lines_a,
            lines_b,
            moves,
            moves_flags,
            cache: Mutex::new(cache),
            _held: [indexes, moves_held, buf_held, cache_held],
        })
    }

    /// The result's header and meta.
    #[must_use]
    pub fn file(&self) -> &SavedFile<R> {
        &self.file
    }

    /// Lines of A, read through chunk checks.
    #[must_use]
    pub fn lines_a(&self) -> &impl LineAccess {
        &self.lines_a
    }

    /// Lines of B, read through chunk checks.
    #[must_use]
    pub fn lines_b(&self) -> &impl LineAccess {
        &self.lines_b
    }

    /// Whether a read of A or B found a chunk that changed.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.lines_a.source().failed() || self.lines_b.source().failed()
    }

    /// The first error a script read met, if any. [`DiffView::hunk`]
    /// reports such a segment as pending.
    #[must_use]
    pub fn error(&self) -> Option<String> {
        self.lock().error.clone()
    }

    /// Whether moved blocks were searched but skipped for the budget, and
    /// whether the list filled up.
    #[must_use]
    pub fn moves_skipped_full(&self) -> (bool, bool) {
        (self.moves_flags[1], self.moves_flags[2])
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Cache> {
        self.cache.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The script of `segment`, read from its block on a cache miss.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] or [`Error::Saved`] from reading the block.
    ///
    /// # Panics
    ///
    /// Panics if `segment` is out of range.
    pub fn try_hunk(&self, segment: usize) -> Result<Option<Arc<Hunk>>> {
        let s = &self.map.segments[segment];
        if s.status == SegmentStatus::Equal {
            return Ok(Some(Arc::new(Hunk {
                edits: vec![Edit::Equal {
                    a: s.a.start,
                    b: s.b.start,
                    len: s.len_a(),
                }],
                approx: None,
            })));
        }
        let j = self
            .map
            .blocks
            .partition_point(|b| b.first <= segment as u64)
            - 1;
        let first = self.map.blocks[j].first as usize;
        let mut c = self.lock();
        if let Some(pos) = c.slots.iter().position(|e| e.0 == j) {
            let slot = c.slots.remove(pos);
            let h = slot.1[segment - first].clone();
            c.slots.insert(0, slot);
            return Ok(h);
        }
        let Cache { buf, .. } = &mut *c;
        let hunks = self.file.block_hunks(&self.map, j, buf)?;
        let size = 16
            + 8 * hunks.len() as u64
            + hunks
                .iter()
                .flatten()
                .map(|h| {
                    crate::sched::HUNK_OVERHEAD.0 + (h.edits.capacity() * size_of::<Edit>()) as u64
                })
                .sum::<u64>();
        let h = hunks[segment - first].clone();
        while c.used + size > c.cap || c.slots.len() == CACHE_SLOTS {
            let Some(old) = c.slots.pop() else { break };
            c.used -= old.2;
        }
        // A block larger than the cache, possible only if the map pass
        // misjudged, is served without being kept.
        if c.used + size <= c.cap {
            c.used += size;
            c.slots.insert(0, (j, hunks.into(), size));
        }
        Ok(h)
    }
}

impl<R: Source, A: Source, B: Source> DiffView for SavedDiff<R, A, B>
where
    R: Send + Sync,
    A: Send,
    B: Send,
{
    fn len_a(&self) -> u64 {
        self.file.meta().lines[0]
    }

    fn len_b(&self) -> u64 {
        self.file.meta().lines[1]
    }

    fn segments(&self) -> &[Segment] {
        &self.map.segments
    }

    fn hunk(&self, segment: usize) -> HunkState {
        match self.try_hunk(segment) {
            Ok(Some(h)) if h.approx.is_some() => HunkState::Approx(h),
            Ok(Some(h)) => HunkState::Ready(h),
            Ok(None) => HunkState::Pending,
            Err(e) => {
                self.lock().error.get_or_insert_with(|| e.to_string());
                HunkState::Pending
            }
        }
    }

    fn progress(&self) -> Progress {
        let n = self.file.meta().differing();
        Progress { done: n, total: n }
    }

    fn approx_refined(&self) -> ApproxTally {
        self.map.approx
    }

    fn set_visible(&self, _range: Range<LineNo>) {}

    fn moved_blocks(&self) -> &[MovedBlock] {
        &self.moves
    }

    fn moves_disabled(&self) -> bool {
        self.moves_flags[0]
    }
}
