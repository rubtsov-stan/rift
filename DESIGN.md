# Rift design

Rift compares text files of tens to hundreds of gigabytes under a hard,
user-set heap limit (`--mem`, default 256 MiB), with instant navigation
through the differences in a terminal interface. This document is the
reference for the algorithm, the memory invariant and the guarantees.
If code and this document disagree, the document wins until it is amended.

## Problem

Classic diff holds both files in memory. GNU diff on two 40 GB files needs
on the order of 100 GB of RAM, so a laptop or a CI runner cannot run it.
Existing workarounds either split the files into fixed chunks and diff each
chunk, which breaks as soon as an insertion shifts a chunk boundary, or
compare files as sets of lines, which loses order and repeats and is no
longer a diff.

Target users compare database dumps before and after a migration, CSV or
NDJSON exports, generated fixtures, large golden test files and
configuration snapshots.

## Core idea

Split the diff into a cheap global skeleton and expensive local refinements,
and let neither exceed its share of the memory budget.

### Strata

Every line gets a 64-bit hash `h` (xxh3). A line belongs to stratum `L` when
the low `L` bits of `h` are zero. Sampling depends only on line content, so
both files sample the same lines, and insertions or deletions elsewhere do
not change which lines are sampled. This is the same principle as
content-defined chunking.

Strata are nested: stratum `L+1` is a subset of stratum `L` and on average
half its size. When a sample does not fit the budget, it is thinned in place
by raising `L`, without rereading the file. To refine a large region, only
that region is reread at a lower stratum.

### Anchors and skeleton

An anchor is a sampled line that occurs exactly once in each file, as in
patience diff. Candidate anchors are confirmed by comparing the two lines
byte by byte; a hash match alone is never trusted. The longest increasing
subsequence over anchor pairs gives the skeleton of the match. The gaps
between consecutive anchors are independent subproblems.

Anchors are verified lazily, during the gap comparison. Each anchor opens
the gap that runs to the next one, so its line is the first line of that
gap. When the gap is compared and found equal, the anchor's bytes were
part of the comparison. When the gap differs, the anchor's two lines are
read on their own by the comparison task that owns the gap: one cold
read per file, since a differing gap is not streamed. An anchor whose
lines differ is demoted: the gap it closes and the gap it opens merge
into one `Differs` segment. A false anchor can
therefore only coarsen the map; no line is ever marked `Equal` without
a byte comparison. Confirming every candidate before the subsequence is
built costs one random read per candidate and file, which on a pair
larger than the page cache amounts to a third pass over the disk. On the
1 GiB generated pair (seed 7, Apple M1, warm cache) it took 1.6 s of a
2.3 s run, even on eight threads. Reading only the anchors of differing
gaps, but after the comparison had finished, took the 10 GiB pair from
13 s to between 23 and 27 s with a cold cache, because those reads ran
one after another on a single thread instead of inside the parallel
comparison tasks.

### Memory invariant

With `B` the budget, at every moment:

```
skeleton + index                              <= B / 2
sum(active refinement work) + hunk cache      <= B / 2
```

The skeleton keeps to its half by choosing the stratum adaptively. Refinements
take memory through a semaphore counted in bytes. A region whose estimated
working set does not fit is split at a lower stratum, or marked approximate.

The budget covers the heap. Files are read through `pread` into fixed-size
buffers, never through `mmap`, so resident memory does not grow with file
pages. A counting global allocator tracks current and peak heap, and tests
assert the peak against `--mem`.

## Architecture

Two zones, each owning half the budget.

The upper zone is built in two passes over the disk and produces the map
for the first screen: scan, strata, sparse line index, anchor selection,
skeleton, streaming equality check of segments.

The lower zone works lazily. The terminal interface requests the visible
region; the unified diff writer walks segments in order through the same
scheduler. Refinement of a segment runs recursive patience diff, then Myers
with linear memory and a cost cutoff.

The core (everything except the terminal interface and output) lives in
`rift-core` and knows nothing about the terminal or the file system. Data
comes through a source trait; tests run on in-memory data.

## Algorithms and data structures

| Stage | Algorithm | Data | Cost |
|---|---|---|---|
| Scan | line ends via SIMD `memchr`, xxh3 per line, parallel over chunks, line numbers by prefix sum | sample records `(hash, line, offset)`, 24 bytes each | O(n) in bytes |
| Position index | checkpoints at most `C` lines apart, placed per scan chunk | sparse `(line, offset)` array | line access scans fewer than `C` lines |
| Anchor selection | occurrence counts, uniqueness in both files, byte comparison against hash collisions | open-addressing hash table preallocated from the budget | O(n / 2^L) |
| Skeleton | longest increasing subsequence (patience sorting) | vector of segments with status Equal / Differs / Approx | O(m log m) in anchors |
| Equality check | streaming comparison of the segment's byte ranges | two fixed buffers | O(n) |
| Segment refinement | recursive patience, then linear-memory Myers (middle snake) with a cost cutoff | local arrays of line hashes | O((N+M)·D) time, O(N+M) memory |
| Oversized segment | resample at stratum `L-k` inside the segment only | same structures, within the refinement budget | recursion over strata |
| Laziness | priority scheduler: visible, then adjacent, then background | priority heap plus LRU cache of finished hunks | evicted hunks are recomputed deterministically |

The first screen costs two linear passes over the disk and O(m log m) in the
number of anchors `m`. Detailed work happens only where the user looks.

## Guarantees

- The output is always a correct edit script: applied to A, it yields
  exactly B.
- Equal segments and anchors are confirmed byte by byte. A hash match is
  never treated as equality.
- Peak heap stays under `--mem`, confirmed by the allocator counter and by
  peak RSS measurement on both platforms.
- Approximate regions are marked explicitly in every output: terminal
  interface, unified diff and summary statistics.
- Exit codes follow `diff`: 0 identical, 1 different, 2 error.

## Non-guarantees

- The diff is not minimal, as with patience diff in git.
- Whole-file row reordering (a dump with a different row order) is outside a
  line-based diff. A key-based comparison mode is a candidate for later.
- Windows is out of scope. The only system call required is `pread`;
  sequential-read hints (`posix_fadvise` on Linux, `F_RDAHEAD` on macOS)
  are optional.

## Blind spots

- Regions without unique lines (identical lines, repeated CSV values). In the
  MVP these fall back to Myers with a cutoff and an Approx mark. After the
  MVP: shingle anchors, hashes of a window of `w` lines.
- Lines longer than the read buffer are hashed in streaming mode and shown
  truncated.
- Hash collisions cause a false match only if the byte check is skipped;
  it never is.

## Hypotheses

The first two decide whether the design holds and are tested by a spike
before the main work.

| Hypothesis | Test | If it fails |
|---|---|---|
| H1. On realistic data (dumps, CSV) at least 99% of lines land in segments that fit the refinement budget | share of unique lines and the distribution of segment sizes on public datasets | bring shingle anchors forward |
| H2. Scanning is bound by the disk, not the CPU | scanner throughput against `cat file > /dev/null` on the same machine | profile, batch the hashing |
| H3. Lazy refinement of the visible hunk fits in 100 ms | key-to-frame latency in the terminal interface | show a coarse hunk first, then refine |

## MVP criteria

The MVP is done when every item below is confirmed reproducibly by a
script in the repository. The numbers are targets; they were set before the
spike and may be revised against its results.

- **Memory.** Two synthetic 50 GB files with 0.1% of lines changed: peak
  heap stays at or below `--mem` (default 256 MiB), confirmed by the
  allocator counter and by peak RSS.
- **Correctness.** At least 10 000 proptest cases in which the edit script
  applied to A yields exactly B, plus differential tests against
  `git diff --no-index --patience` on a corpus of small files.
- **Compatibility.** The unified diff output applies with `patch` without
  errors, Approx regions included. Exit codes follow `diff`: 0, 1, 2.
- **Interactivity.** The difference map appears after two passes over the
  disk. The visible hunk is ready within 100 ms of a jump when its segment
  fits the budget.
- **Performance.** `--summary` takes at most 2.2x the time of sequentially
  reading both files on the same machine. Equal segments are byte-verified,
  so the design reads both files twice: once to scan, once to compare the
  stretches between anchors. 2x is its floor, not a reachable target.
  Met: on a 10 GiB pair (Apple M1, seed 7) `--summary` takes 2.03x to
  2.13x the time of `cat a b` over three alternating rounds.
- **Honesty.** Approximate regions are marked in the terminal interface and
  in the final statistics.
- **Packaging.** One binary for macOS and Linux, installed with one command;
  a README with the design, the invariants and reproducible benchmarks.

## Crates

The core (scanner, strata, skeleton, Myers, budget) is written in this
repository; that is the engineering value of the project. Everything around
it uses established crates: `memchr`, `xxhash-rust`, `rayon`, `clap`,
`thiserror`, `anyhow`, `libc`; `ratatui` and `crossterm` for the terminal
interface; `proptest`, `imara-diff`, `divan` and `tempfile` for tests and
benchmarks.

`unsafe` is not needed for the MVP. It is allowed only where a profile proves
the gain, with a `// SAFETY:` comment.

## Module contracts

Shared types live in `rift-core` and are frozen before parallel work starts.
Every module below meets the others only through these types.

**`LineNo`, `ByteOffset`, `Bytes`** (`types`). Zero-based line number,
byte position and byte count, each a `u64` newtype so the compiler catches
a mix-up. `LineHash` is the 64-bit xxh3 of a full line, terminator included:
`a\n`, `a\r\n` and an unterminated `a` are three different lines, as in
GNU diff.

**`Source`** (`source`). Positional reads with a known length, modelled on
`pread`: `read_at(buf, ByteOffset)` may return short, zero means end. The
only way the core sees bytes. Implemented for `[u8]` in the core and for
`File` in `rift-cli`.

**`LineAccess`, `LineBuf`, `MemLines`** (`lines`). `read_lines(range, buf)`
fills a caller-owned `LineBuf` with the bytes of consecutive lines,
terminators included, reusing its two vectors, so a loop over lines
allocates nothing after the first call. `read_line_part(line, from, max,
buf)` returns `max` bytes of a line from byte `from`, so a line longer than
any buffer can be written or compared in pieces. `MemLines` indexes a byte
slice for tests. A trailing unterminated line counts as a line; an empty input has
none.

**`Budget`, `Half`, `Reservation`** (`budget`). The memory invariant as a
type. `Budget::new(total)` gives each half `total / 2`. `try_reserve(half,
bytes)` either returns a `Reservation` or `BudgetExceeded` with the bytes
still available, and the counter never reads above the capacity, not even
transiently. A reservation can `grow` and `shrink` and gives everything
back on drop. Allocation on the data path happens only after a reservation.

**`Sample`** (`sample`). `(hash, line, offset)`, 24 bytes, pinned by a
const assertion. A 128 MiB half holds 5.6 million.

**`Segment`, `SegmentStatus`** (`segment`). A pair of line ranges with the
same meaning in both files. Consecutive segments tile both files without
gaps or overlap; either range may be empty, never both. `Equal` means
confirmed byte by byte, `Differs` means an exact local diff is possible,
`Approx` means the local diff was cut short.

**`Edit`, `Hunk`** (`edit`). `Equal { a, b, len }`, `Delete { a, len }`,
`Insert { b, len }` in absolute line numbers. A hunk's edits are ordered
and contiguous: walking them visits every A line of the segment once
through `Equal` and `Delete`, every B line once through `Equal` and
`Insert`, both in increasing order. Replaying them against A yields B. An
`approx` hunk is still exact in that sense, only not minimal.

**`DiffView`, `HunkState`, `Progress`** (`view`). What a front end sees:
line counts, the segment list, `hunk(i)` returning `Ready`, `Pending` or
`Approx` without blocking, a `done / total` progress, and `set_visible`
with the A lines on screen so the scheduler can prioritize.

**`Error`** (`error`). One enum for the crate: I/O, budget exceeded, line
too long for its buffer, line out of range, invalid edit script.

**Counting allocator** (`alloc`, feature `counting-alloc`). A wrapper over
the system allocator with current and peak counters. `rift-cli` installs it
as the global allocator; tests and `--summary` read the peak.

### Sizing notes from the spike

- The starting stratum `L` is chosen from file size with a pessimistic
  bytes-per-line guess, and the scanner relies on in-place thinning when the
  guess is wrong. At 82 bytes per line a 10 GiB pair already needs `L = 6`
  for samples alone and `L = 7` once the anchor table and index share the
  half.
- Thread count is sized by per-line hashing cost, about 70 ns per short
  line on one thread, not by disk bandwidth.

## Spike results

Measured on 2026-11-03. Hardware: Apple M1 (4 performance + 4 efficiency
cores), 16 GiB RAM, internal SSD, macOS 27.0.1, rustc 1.97.1, release
profile from the workspace, no `target-cpu=native`. The spike code is
`crates/rift-core/examples/spike.rs`; data sets and derived files live
outside the repository.

### Data sets

| Pair | Source | Lines | Bytes | B derived by |
|---|---|---|---|---|
| pagila | Pagila `pagila-data.sql` + `pagila-insert-data.sql` (pg_dump) | 245 628 | 32.6 MB | `spike mutate` seed 1, ratio 0.001 |
| tlc | NYC TLC yellow taxi 2015-01, parquet converted to CSV with pyarrow | 12 741 036 | 1.32 GB | same |
| gh | GH Archive 2024-01-15 hours 10 to 13, NDJSON | 1 054 597 | 3.74 GB | same |
| gen-csv | `rift-gen --seed 1 --size 1G --profile csv` | 13 101 706 | 1.07 GB | generator manifest |
| gen-sql | `rift-gen --seed 2 --size 1G --profile sql` | 13 287 347 | 1.07 GB | generator manifest |

`spike mutate` applies insert, delete and modify in rotation at a given
per-line probability, so the real pairs have 0.1% changed lines.

### H1: anchor coverage

Command: `spike anchors a b`. Stratum `L` keeps hashes whose low `L` bits
are zero. A gap is the region between consecutive anchors after the longest
increasing subsequence; its refinement is assumed to need 32 bytes per line
of A plus B.

Share of lines unique within one file, and unique in both:

| Pair | unique in A | unique in both |
|---|---|---|
| pagila | 0.9954 | 0.9948 |
| tlc | 1.0000 | 0.9993 |
| gh | 1.0000 | 0.9994 |
| gen-csv | 0.9936 | 0.9865 |
| gen-sql | 0.9931 | 0.9859 |

Anchors and gap sizes at the stratum a 256 MiB budget would force on a
10 GiB pair (`L = 6`, see below), and at a far coarser one:

| Pair | L | sample size (both files) | anchors in LIS | gap lines p50 / p99 / max | largest gap bytes |
|---|---|---|---|---|---|
| pagila | 6 | 0.2 MiB | 3 801 | 86 / 577 / 964 | 0.2 MiB |
| tlc | 6 | 9.1 MiB | 198 584 | 86 / 591 / 1 536 | 0.1 MiB |
| gh | 6 | 0.8 MiB | 16 498 | 88 / 578 / 1 206 | 2.6 MiB |
| gen-csv | 6 | 9.3 MiB | 201 085 | 88 / 612 / 1 703 | 2.0 MiB |
| gen-sql | 6 | 9.8 MiB | 204 968 | 88 / 608 / 1 720 | 1.9 MiB |
| tlc | 14 | 0.04 MiB | 761 | 26 817 / 145 313 / 246 841 | 12.4 MiB |
| gh | 14 | 0.00 MiB | 68 | 20 482 / 130 102 / 136 317 | 238.9 MiB |

On every pair and every `L` from 0 to 14, 100% of lines fall in gaps whose
refinement fits 16 MiB, let alone the 128 MiB half budget. The largest gap
at `L = 14` is 246 841 lines, about 7.5 MiB of refinement memory. The
generated pairs have 1 000-line gaps already at `L = 0`: those are the
duplicated and low-uniqueness runs the generator plants on purpose.

Budget check for the milestone target. The 10 GiB generated SQL pair has
131.6 million lines per file. With 24-byte samples, `L = 6` gives
4.1 million samples for both files, 99 MiB, under the 128 MiB skeleton half
with room for the line index. `L = 7` gives 49 MiB.

H1 holds on these data sets. The caveat: every set has a unique id per row,
so this says nothing about dumps without one. The blind spot stays as
described above, and shingle anchors stay after the MVP.

### H2: scan throughput

Command: `spike scan file`. Each line is found with `memchr` and hashed with
xxh3-64. The parallel variant splits the file into 8 MiB chunks aligned to
line ends. Baseline is `cat file > /dev/null` timed with `/usr/bin/time -p`.

`purge` needs root, so the cold-cache run used eviction instead: the 10 GiB
file was read after reading the other 10 GiB file of the pair twice, on a
machine with 16 GiB of RAM. The two `cat` runs agree, which is consistent
with both being cold.

Cold page cache, 10 GiB generated SQL, 131.6 million lines of 82 bytes:

| Run | Time | Throughput |
|---|---|---|
| cat | 3.30 s, 3.54 s | 3.25, 3.03 GB/s |
| scan, 1 thread | 9.24 s | 1.16 GB/s |
| scan, 2 threads | 5.67 s | 1.89 GB/s |
| scan, 4 threads | 3.24 s | 3.32 GB/s |
| scan, 8 threads | 3.13 s | 3.43 GB/s |

Warm page cache:

| File | cat | 1 thread | 8 threads |
|---|---|---|---|
| tlc, 104 bytes per line, 1.32 GB | 6.31 GB/s | 2.64 GB/s | 6.91, 8.23 GB/s |
| gh, 3.5 KB per line, 3.74 GB | 5.59 GB/s | 5.17 GB/s | 7.64, 6.90 GB/s |

H2 holds for the parallel scanner: with four or more threads it reads at
disk speed. A single thread is CPU-bound on short lines at about 70 ns per
line, 14 million lines per second, so the scanner must be parallel, as
planned.
