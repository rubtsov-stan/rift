# Rift design

Rift compares large text files under a hard, user-set heap limit
(`--mem`, default 256 MiB), with instant navigation through the
differences in a terminal interface. The largest pair measured so far is
10 GiB. This document is the
reference for the algorithm, the memory invariant and the guarantees.
If code and this document disagree, the document wins until it is amended.

## Problem

Classic diff holds both files in memory. GNU diff did not run on a 10 GiB
pair on a machine with 16 GB of RAM, so a laptop or a CI runner cannot
run it.
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
with linear memory and a cost cutoff; pieces of 12 lines or fewer go to a
quadratic DP table instead.

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
| Segment refinement | recursive patience, then linear-memory Myers (middle snake) with a cost cutoff; a piece of 12 lines or fewer (A plus B, after its common prefix and suffix) goes to a quadratic DP table | local arrays of line hashes | O((N+M)·D) time, O(N+M) memory |
| Oversized segment | resample inside the segment only, at the lowest stratum below the parent's whose samples fit, with unique-line and shingle anchors | same structures, within the refinement budget | recursion over strata |
| Laziness | scheduler in a fixed order: visible segments, then the 4 after and the 4 before them (nearest first), then segments a front end asked for by number, lowest first, then the rest in file order from a cursor; `Diff::write_unified` turns that last stage off | one row per segment, linked into an LRU list of finished hunks, and a sorted set of requested segments | evicted hunks are recomputed deterministically, on request |

The first screen costs two linear passes over the disk and O(m log m) in the
number of anchors `m`. Refinement starts where the user looks; the rest
runs in the background.

## Guarantees

- The output is always a correct edit script: applied to A, it yields
  exactly B.
- Equal segments and anchors are confirmed byte by byte. A hash match is
  never treated as equality.
- Peak heap stays under `--mem`. CI checks it on macOS and Linux with
  `bench/ci-mem.sh`, which asserts the counting allocator's peak heap
  against `--mem` for `--summary`, the full diff and key mode. The script
  prints peak RSS but does not assert it.
- Approximate regions are marked explicitly in every output: terminal
  interface, unified diff and summary statistics.
- Exit codes follow `diff`: 0 identical, 1 different, 2 error.

## Non-guarantees

- The diff is not minimal, as with patience diff in git.
- Whole-file row reordering (a dump with a different row order) is outside a
  line-based diff. Key mode (0.2) compares such files by record; the line
  diff still shows a moved block as a deletion and an insertion.
- Windows is out of scope. The only system call required is `pread`;
  sequential-read hints (`posix_fadvise` on Linux, `F_RDAHEAD` on macOS)
  are optional.

## Blind spots

- Regions without unique lines (identical lines, repeated CSV values). In
  0.1 these fell back to Myers with a cutoff and an Approx mark. Since 0.2,
  `anchors::select` adds shingle anchors, hashes of a window of `w`
  samples, in the gaps unique lines leave (see "Shingle anchors"). A
  segment too large to refine in one job is resampled inside itself at a
  lower stratum and cut at the anchors found there, recursively. A piece
  becomes Approx only when no stratum below its parent's gives it an
  anchor, when Myers on a piece that fits passes its cost cutoff, or
  when the job's own budget runs out, which turns the rest of the
  segment into one replacement.
- Lines longer than the read buffer are hashed in streaming mode. The
  unified diff writes them whole, in pieces of 64 KiB; only the terminal
  view and the excerpts of `rift-mcp` and `rift serve` cut them for
  display.
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

## MVP criteria (0.1)

The MVP is done when every item below is confirmed reproducibly by a
script in the repository. The numbers are targets; they were set before the
spike and may be revised against its results.

- **Memory.** Two synthetic 50 GB files with 0.1% of lines changed: peak
  heap stays at or below `--mem` (default 256 MiB), confirmed by the
  allocator counter and by peak RSS. The largest pair run so far is
  10 GiB; no 50 GB pair has been run.
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

`unsafe` is allowed only where a profile proves the gain or std has no safe
call, with a `// SAFETY:` comment. SECURITY.md lists every block.

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
too long for its buffer, line out of range, invalid edit script, `Keyed`
when key mode cannot compare the files (it wraps a `KeyError`), and
`Cancelled` when a scan was stopped through its `Watch`.

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
described above; shingle anchors, added in 0.2, address it.

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

## 0.2

0.2 added four features to the MVP: shingle anchors, key mode, moved blocks
and an MCP server. The contracts they share went in first, so each feature
stays inside its own module and none of them edits a shared file. A
performance pass over the skeleton followed shingle anchors. The
persistent index was left out of 0.2: it writes files next to user data
and needs its own design for safety and invalidation.

### Contracts

**`CandidateSource`, `UniqueLines`** (`anchors`). A source gets the
samples of both files and the budget, and returns `Anchors`: pairs of
sample indices, strictly increasing in A, with no B index used twice, and a
skeleton-half reservation that covers them. Candidates are hash matches;
the skeleton confirms the ones it keeps. `anchors::select` alone decides
which sources run, and both the skeleton and the resampler call it. It
runs `UniqueLines`, then adds shingle anchors in the gaps they leave.

**`OffChain`** (`skeleton`). The candidates the chain search leaves out,
as `[line in A, line in B]` pairs in A order, from `Skeleton::off_chain`,
`Skeleton::into_parts` and `Diff::off_chain`. Nothing has read their
bytes, so a reader compares the two lines before it relies on a pair. The
list costs 16 bytes per pair from the skeleton half and grows as pairs
arrive. It stops at 1/8 of what that half has free when the search starts,
which leaves room for the segment list and the comparison buffers, and
counts the pairs it could not keep in `dropped`.

**`KeySpec`, `CsvColumn`, `KeyedChange`, `ChangeKind`, `KeyError`**
(`keyed`). A key is a CSV column by position or header name, an NDJSON
field path, or a column of a `pg_dump` COPY block by name. `--key` parses
`csv:N` (counted from 1), `csv:NAME`, `json:a.b` and `copy:NAME`. A
`KeyedChange` holds the key's bytes as the file has them, `Added`,
`Removed` or `Changed`, and the record's first line in A and in B; a side
is absent only when the kind says so. `KeyError` reaches callers as
`Error::Keyed`, and its first variant is duplicate keys. `rift --key`
prints the changes described under "Key mode".

**`MovedBlock`** (`moves`). Two line ranges of the same length, one in A
and one in B, holding the same bytes. `DiffView::moved_blocks` returns
them ordered by A, and its default returns none. A moved block annotates
the diff and never changes the edit script.

**`rift-mcp`**. A crate with an MCP server on standard input and output,
built on `rmcp`. It answers `initialize` and serves the tools described
under "MCP server". The workspace manifest lists what the server needs:
`rmcp`, `tokio`, `serde`, `serde_json` and `schemars`. `rift-core` gains
no dependency.

**Generator profiles** (`rift-gen`). `nokey` draws every row from a pool
of 1024 rows without an id, so in a file of a few MiB no line occurs only
once. `shuffled` gives every row a unique id in the first column (the `id`
field for NDJSON) and moves blocks of 1 to 50 rows in B, each across at
least four times its length; inserts, deletes and modifies act on keys,
and a modify keeps its key. `--rows csv|sql|ndjson` picks the row format.
The manifest lists every change and every block move with its line in A
and in B.

### Shingle anchors

A dump without an id column gives the skeleton few unique lines, so it
ends up with few anchors and large `Approx` regions. A shingle is the hash
of a window of `w` consecutive lines, combined from line hashes instead of
hashing the bytes again. Shingles are sampled by the strata rule, and one
that occurs once in each file becomes a candidate pointing at its first
line. They come from a second `CandidateSource` and are paid from the
skeleton half; when they do not fit, the skeleton falls back to unique
lines alone and does not fail. A shingle anchor is confirmed like any
other, by comparing its first line byte by byte; the rest of its window
lies in the gaps after it, which are compared or refined.

A window is `w = 3` consecutive samples, not lines, so it needs nothing
the scan does not already keep. Its hash also covers the line count and
byte length between its samples, which gives a window enough entropy at a
high stratum, where few distinct rows are sampled. On a 1 GiB `nokey` CSV
pair at stratum 6, windows of 2, 3, 4, 6, 8 and 16 samples gave 90k, 254k,
215k, 156k, 114k and 34k anchors: longer windows break on more edits, and
16 left an `Approx` region. Shingles run only in gaps of at least 32
samples between unique-line candidates. Such gaps hold none of the samples
of tlc and gh, 0.08% of pagila, 0.34% of the 1 GiB `sql` pair, and all of
them on `nokey`. Run everywhere, shingles made pagila, tlc and the `sql`
pair 19 to 33% slower and found no extra anchor there. With the gap rule
those pairs stay within run-to-run noise of main, and on `nokey` the share
of lines in `Approx` regions drops from 100% to 0 on the 64 MiB CSV, SQL
and NDJSON pairs and the 1 GiB CSV pair, which takes 3.6 s instead of
12.9 s (medians of 5, machine under load). Peak heap is unchanged on every
pair measured.

### Key mode

`rift --key SPEC a b` compares records by key, in any row order, and
prints the added, removed and changed keys with their line numbers. Each
pass owns a range of key hashes and reads A and B side by side, a window
of each at the same relative position, skipping records outside its
range. A record waits in memory, bytes included, until its key turns up
in the other file; the two records are then compared byte by byte and
only a 31-bit fingerprint of the key hash stays, in a 4-byte table slot,
to catch a third occurrence. At 80% load that is 5 bytes per key, and
files in roughly the same order keep few records waiting. The tables
come from the skeleton half; the windows, their record lists and the
waiting records from the refine half. Each pass rereads both files and
writes nothing to disk.

The work is spread over threads twice. Each window is cut into chunks
at line breaks and every chunk is split into records on its own thread,
assuming a record starts at its first byte and the parser is in the
state the window started in. A chunk counts only if the one before it
ended where it starts, in that state; a quoted line break at a cut, or
a `COPY` header or terminator inside the window, makes the rest parse
again from there. The pass's range is then split between shards, one
per thread, by bits of the key hash the table does not use, and each
shard takes its records with a table and waiting records of its own.
Under 16 MiB of budget one thread does all of it. With more, the thread
pool's own heap is reserved from the refine half, 16 KiB per thread.

A fingerprint cannot tell which keys lie above a new top of the range,
so when a shard's table or waiting records fill up the pass starts over
on a narrower range. The tables fill as the files are read, so when
they overflow the new range keeps the share that leaves them 90% full
at the end: `0.9 × progress / fill` of the old one. Waiting records do
not pile up that way, so when they overflow the refine half the new
range keeps the smaller of that share and one half: it is at least
halved and can shrink further. That overflow also sets a ceiling of 90%
of the range that overflowed. A pass that ends with its tables under
90% full widens the next one toward 90%, at most twofold and never
above the ceiling. On the 10 GiB `shuffled` CSV pair (seed 7,
`--mem 256M`, Apple M1, files larger than the page cache) this takes
6 passes and about 41 s, against 13 passes and 157 to 231 s before;
each pass now runs at the speed of the disk, about 3.2 GB/s.

A key found twice while its first record waits is a duplicate. A key
whose fingerprint matches a matched key is only a suspect, since the
matched key's bytes are gone; the next pass, or one more read when
there is none, counts the suspects in their file, so a duplicate can
surface after the changes of its pass were printed. A pass that piles
up too many suspects stops and counts them at once. The error lists the
first few duplicates. Storing A's records whole, as a plain partition
by key hash would, costs about 28 times more memory per record on
`shuffled` CSV (138 bytes against 5 for an 81-byte row) and as many
more passes. A record counts as unchanged only after its bytes compare
equal. `shuffled` is the test bed: its manifest names every
change, and a pair that is only reordered reports none.

### Moved blocks

A block that moved shows up as a deletion and an insertion. Its lines
still match between the files, and the chain search leaves those matches
out because they cross the chain: they are the `OffChain` pairs.
`moves::Moves::find` takes each pair as a seed and compares its two lines
byte by byte, since a pair is only a hash match. It then extends the run
in both directions, 16 lines per side and read, while the lines are equal
and neither lies in an `Equal` segment. Runs of at least 3 lines are kept.
A seed inside a block already found adds nothing, so consecutive pairs
form one run, and a seed costs two reads. On the generated profiles, with
and without moves, and on the real data sets, no block came out false at
any length, so the minimum of 3 only drops moves of one or two lines,
which read well as a plain deletion and insertion. Seeds are the
`OffChain` pairs: the unique-line and shingle candidates from
`anchors::select` that cross the chain, kept up to 1/8 of the skeleton
half that is free when the chain search starts. Every seed is a sampled
line, so at stratum `L` a block of `n` lines has no seed, and goes
unreported, with chance at least `(1 - 2^-L)^n`. On `gen1g`
(stratum 4) that leaves 424 of the 1 062 moves of 3 lines or more found.

Memory comes from the refine half and is taken before the scheduler
takes what is left: 32 bytes per block for the life of the diff, capped
at 1/8 of what the half has free, and while the search runs two readers
with 4 KiB buffers and two comparison buffers of 128 KiB. When either does
not fit, the search keeps nothing and the summary says moves were not
searched. `--no-moves` (`diff::Config { moves: false }`) skips the search
and takes no memory for it; `--summary` and `--stats` say so, and the
terminal status line shows `moves off`. `--summary` and `--stats` print the block count, the first ten
blocks as `a` and `b` ranges, and how many candidates the skeleton
dropped, when it dropped any. The terminal view marks both sides of a
moved block with `m` and a colour of their own, and `m` jumps from the
first moved row on screen to the other end of its block. The unified diff
stays byte-identical.

### MCP server

`rift-mcp` lets an MCP client explore the diff of two large files through
small answers instead of megabytes of text: `summary`, `hunks_at` and
`next_difference`. Each pair of files has one `Diff` that stays alive
between calls, so the skeleton is built once. At most four diffs are
alive and their `--mem` budgets sum to at most 512 MiB, which bounds the
server's accounted heap. A new pair evicts idle diffs first, then the
least recently used one that no call holds, and fails with "busy" when
every diff is in use. A diff idle for five minutes is dropped. The server
opens regular files only, read-only, and refuses symbolic links in the
last path component. Every answer has a size bound: 20 ranges in a
summary, and at most 500 lines, 64 KiB and 256 bytes per line of diff
text. It reads a segment's script through `DiffView` and renders it
itself; the ranges it reports come from segments, so they can be a few
lines wider than the change.

### Performance

The performance pass profiles the full diff and `--summary` on a 10 GiB
pair and targets the second pass over differing regions. It starts after
shingle anchors, since both change the skeleton's comparison path.

What the pass changed, measured on an Apple M1 with warm cache unless
stated. The serial part of the skeleton shrank: the chain search reads
its tails from a dense array and skips the search when a candidate
extends the longest chain, the unique-line table is filled by every
thread, each owning a range of its slots, and shingles find their gaps
without copying or sorting the candidates. The scheduler wakes one
worker per new job instead of every thread, and the unified writer
reuses the bytes its last read left in the buffer and moves the visible
window a quarter of its length at a time. Together, on the 1 GiB `sql`
pair, `--summary` at `--mem 1G` went from 0.81 to 0.57 s and the full
diff from 0.99 to 0.67 s at `--mem 1G` and from 0.58 to 0.45 s at the
default 256 MiB (medians of 6); pagila takes 34.5 ms for `--summary` and 37.6 ms for
the full diff, against 45.9 and 52.4 ms before (means of 50 runs). A
larger budget still costs more than a smaller one, since it picks a
lower stratum and every stage handles more samples, for a finer map.
On the 10 GiB pair, which is larger than the page cache, both modes are
bound by the disk: `--summary` takes 2.2 times `cat` and the full diff
spends 93% of its samples in `pread`, mostly the reads of differing
regions.

## Clients (0.3)

`rift-mcp` ships in 0.2. The protocol, `rift serve`, `rift-session` and the
VS Code extension ship in 0.3.

One protocol serves every client that is not the terminal interface:
`rift serve`, JSON-RPC 2.0 over standard input and output with LSP's
`Content-Length` framing, specified in `docs/protocol.md` with a JSON
schema per message under `docs/protocol/`. It is a subcommand of the
`rift` binary, so a client ships or finds one executable. Two clients
use it or are planned on it:

- **The VS Code extension** (`editors/vscode/`) never opens the big
  files in an editor. It lists the differing segments and moved blocks
  in a tree, asks for one segment's lines on both sides with context,
  and opens VS Code's own diff editor on those two excerpts, titled
  with their line ranges in the original files.
- **`rift-mcp`** answers MCP clients with the same sessions. It keeps
  its MCP transport and renders its own unified text, and moves onto
  the protocol once that covers what its tools return.

The session logic lives once, in `rift-session`: one diff per pair of
files, opened read-only after the path checks of the MCP server, cached
while the files are unchanged, dropped when idle or for room, and
bounded in count and in the sum of budgets. It is synchronous. The core
has nothing to await: scanning is two passes of `pread`, refinement runs
on the scheduler's threads, and an excerpt is a bounded number of reads.
`rift serve` reads requests on one thread and answers each on its own;
`rift-mcp`, whose MCP library runs on tokio, calls in from
`spawn_blocking`.

A session that `rift serve` opens scans on a thread of its own, so
`open` answers at once. The core's scan takes a `Watch`: it counts the
bytes both passes have covered, which the server reports as progress,
and is checked before every read, so closing the session stops the scan
within one chunk per scan thread and frees its memory then. One thread
looks at every session each 50 ms and sends what changed, which caps
notifications at 20 a second per session. The server keeps 8 MiB of
`--max-total-mem` for its own buffers and gives the rest to sessions:
request bodies are capped at 64 KiB, and every message is written
through one fixed buffer after a pass that counts its length.

Every response has a bound that does not depend on the files: pages of
at most 1000 segments or blocks, which borrow from the map, and
excerpts of at most `max_lines` lines per side and `max_line_bytes`
bytes per line. An excerpt's lines and text are built inside a response
pool taken from the session's Refine half when it opens, 1/16 of the
half and at most 8 MiB, so the budget covers responses as it covers the
diff, and a larger `--mem` allows larger excerpts.

Line text crosses the protocol as UTF-8 with invalid sequences
replaced and the line flagged `lossy`, with its terminator named apart.
Every client shows text, none applies a patch from an excerpt, and an
excerpt cut at `max_line_bytes` is not exact anyway; the byte-exact
edit script stays in `rift`'s own output.

Errors are typed from the core to the client. Each has a JSON-RPC code
in Rift's range and structured data; a budget refusal carries the
`--mem` to reopen with, computed by `BudgetExceeded::mem_advice`, the
same function behind the CLI's `rerun with --mem`.
