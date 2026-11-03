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
| Position index | checkpoint offset every `C` lines | sparse offset array | line access scans at most `C` lines |
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

## Crates

The core (scanner, strata, skeleton, Myers, budget) is written in this
repository; that is the engineering value of the project. Everything around
it uses established crates: `memchr`, `xxhash-rust`, `rayon`, `clap`,
`thiserror`, `anyhow`, `libc`; `ratatui` and `crossterm` for the terminal
interface; `proptest`, `imara-diff`, `divan` and `tempfile` for tests and
benchmarks.

`unsafe` is not needed for the MVP. It is allowed only where a profile proves
the gain, with a `// SAFETY:` comment.
