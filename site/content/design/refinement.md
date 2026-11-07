+++
title = "Refinement: Myers per segment, on demand, in a budget"
description = "How Rift diffs one differing segment at a time with patience and linear-space Myers, schedules the work around what you look at, and marks approx regions."
weight = 3
[extra]
nav = "Refinement"
h1 = "Refinement"
lead = "The skeleton says where the files differ. Refinement turns one differing segment into a line-level edit script, and only when an output needs it."
summary = "Linear-space Myers per segment, the scheduler and the visible range, and when a segment comes out approximate."
+++

[DESIGN.md](https://github.com/rubtsov-stan/rift/blob/main/DESIGN.md) describes this stage under "Architecture" and in the table "Algorithms and data structures".

## One segment at a time

A refinement job takes one `Differs` segment. It reads the segment's lines on both sides through the line index, hashes each with xxh3, and hands the two arrays of hashes to the `Refiner`. Between anchors the segments are short: in the DESIGN.md spike at stratum 6, the median gap held 86 to 88 lines and the largest 1,720, on every data set measured. So the expensive algorithm runs on small inputs, many times, instead of once on the whole file.

The `Refiner` works in four steps, each on the output of the one before.

1. **Strip.** Lines with equal hashes at the start and at the end of the gap stay as they are. Most gaps end here or nearly.
2. **Patience.** Lines unique in both sides of what is left become anchors, the longest chain of them cuts the gap, and each piece recurses, up to 32 levels deep.
3. **Myers.** A piece with no unique line goes to Myers' algorithm in its linear-space form: find the middle snake, split there, recurse on both halves. Memory stays O(N+M); time is O((N+M)·D) for D edits.
4. **Small pieces.** A piece of 12 lines or fewer goes to a quadratic table instead, which the code comment on `DP_MAX` justifies with a measurement: thresholds from 4 to 12 ran within noise of Myers alone.

The result is two arrays of flags, lines removed from A and lines added to B. The unflagged lines pair up in order, and `verify` compares every pair byte by byte, 64 lines per read. A pair whose bytes differ, which takes two lines with one 64-bit hash, becomes a one-line replacement. The script stays correct, at the cost of one line of minimality.

Sources: `crates/rift-core/src/diff/mod.rs`, `refine` and `hashes`; `crates/rift-core/src/refine/mod.rs`, `Refiner::refine`, `Diff::solve`, `Diff::reduce`, `verify`, `DP_MAX`, `MAX_DEPTH`; `crates/rift-core/src/refine/myers.rs`, `Diff::myers` and `middle_snake`.

## The cost cutoff

Myers is fast when the two sides are similar and slow when they are not. Each piece that reaches Myers gets a limit of `(N+M) × 256` hash comparisons, 256 being the floor git uses. A piece that runs past it is emitted as one replacement, all of its A lines deleted and all of its B lines inserted, and the hunk is marked approximate. The replacement is a valid edit script, only not minimal.

Sources: `diff/mod.rs`, `CUTOFF`; `refine/mod.rs`, `Diff::solve` (the `limit`); `refine/myers.rs`, `Diff::myers`.

## The scheduler and the visible range

<figure class="diagram">
<svg viewBox="0 0 720 230" role="img" aria-labelledby="sch-t sch-d">
<title id="sch-t">The order in which the scheduler refines segments</title>
<desc id="sch-d">A row of sixteen segments. Segments 6 to 8 are on screen and are refined first. Then the four segments after the screen, 9 to 12, then the four before it, 5 down to 2. Then segments a front end asked for by number. Then, if background work is on, every remaining segment in file order from a cursor at the start.</desc>
<text x="20" y="22" font-weight="700">segments in file order</text>
<rect class="box" x="20" y="40" width="40" height="30"/><rect class="box" x="62" y="40" width="40" height="30"/><rect class="box" x="104" y="40" width="40" height="30"/><rect class="box" x="146" y="40" width="40" height="30"/><rect class="box" x="188" y="40" width="40" height="30"/><rect class="box" x="230" y="40" width="40" height="30"/>
<rect class="box" x="272" y="40" width="40" height="30" style="stroke:var(--accent);stroke-width:3"/><rect class="box" x="314" y="40" width="40" height="30" style="stroke:var(--accent);stroke-width:3"/><rect class="box" x="356" y="40" width="40" height="30" style="stroke:var(--accent);stroke-width:3"/>
<rect class="box" x="398" y="40" width="40" height="30"/><rect class="box" x="440" y="40" width="40" height="30"/><rect class="box" x="482" y="40" width="40" height="30"/><rect class="box" x="524" y="40" width="40" height="30"/><rect class="box" x="566" y="40" width="40" height="30"/><rect class="box" x="608" y="40" width="40" height="30"/><rect class="box" x="650" y="40" width="40" height="30"/>
<text x="32" y="60" style="font-size:12px">0</text><text x="74" y="60" style="font-size:12px">5</text><text x="116" y="60" style="font-size:12px">3</text><text x="158" y="60" style="font-size:12px">3</text><text x="200" y="60" style="font-size:12px">3</text><text x="242" y="60" style="font-size:12px">3</text><text x="284" y="60" style="font-size:12px">1</text><text x="326" y="60" style="font-size:12px">1</text><text x="368" y="60" style="font-size:12px">1</text><text x="410" y="60" style="font-size:12px">2</text><text x="452" y="60" style="font-size:12px">2</text><text x="494" y="60" style="font-size:12px">2</text><text x="536" y="60" style="font-size:12px">2</text><text x="578" y="60" style="font-size:12px">5</text><text x="620" y="60" style="font-size:12px">5</text><text x="662" y="60" style="font-size:12px">5</text>
<path class="link" d="M272 84 V92 H396 V84"/><text x="272" y="110" style="font-size:12px">on screen</text>
<path class="link" d="M398 84 V92 H564 V84"/><text x="420" y="110" style="font-size:12px">4 after</text>
<path class="link" d="M104 84 V92 H270 V84"/><text x="130" y="110" style="font-size:12px">4 before</text>
<text x="20" y="146" style="font-size:12px">1: visible, 2: neighbours after, 3: neighbours before, nearest first</text>
<text x="20" y="168" style="font-size:12px">4: asked for by number, 5: background, in file order from a cursor</text>
<text x="20" y="190" style="font-size:12px">0: an equal segment needs no job</text>
<text x="20" y="214" style="font-size:12px">moving the screen cancels running prefetches that are no longer near it</text>
</svg>
<figcaption>The number in each cell is its place in the order. Segment 0 is equal and needs no job. No segment here was asked for by number, so the rest wait for background work.</figcaption>
</figure>

Refinement jobs run on a small pool of threads of the scheduler's own. They are not rayon tasks, so a worker that waits for budget blocks nothing else. Each idle worker takes the first segment in this order that is not refined yet:

1. segments that overlap the lines on screen;
2. the four segments after them, then the four before, nearest first;
3. segments a front end asked for by number and that are not cached;
4. if background work is on, the next segment in file order from a cursor.

The terminal view reports the A lines on screen through `set_visible`. When the screen moves, jobs started as prefetch for segments no longer near it are cancelled; whatever they return is discarded. The unified diff writer has no screen, so it turns background work off and moves the visible window ahead of itself instead: eight segments per worker, moved a quarter of that length at a time, because each move takes the lock the workers share. Background work would race ahead without bound, fill the cache and evict hunks the writer had not reached yet.

Sources: `crates/rift-core/src/sched/mod.rs`, `Queue::near`, `Queue::pick`, `Scheduler::set_visible`, `Scheduler::set_background`, `visible_segments`, `worker`, `NEIGHBORS`; `diff/mod.rs`, `Diff::write_unified`, `AHEAD_PER_WORKER`.

## Memory for refinement

The scheduler lives in the refine half, in what the front end left free. It reserves a row table, then splits the rest in two equal parts: the hunk cache, reserved at once, and `work_cap`, the most that running jobs may reserve together. So active jobs plus the cache never pass the half.

A job reserves its estimate before it starts: the line hashes at 8 bytes per line, the refiner's workspace, and the hunk at one edit per line. When the reservation fails, the worker waits for a running job to return its bytes. The top job waits rather than letting a smaller one jump ahead. The cache drops the least recently used hunk when it needs room. An evicted hunk is computed again when someone asks for it, with the same result, since a job depends on its segment alone.

Sources: `sched/mod.rs`, `Scheduler::new`, `worker`, the `Job` trait; `crates/rift-core/src/sched/cache.rs`, `Cache::insert`, `Cache::evict_lru`; `diff/mod.rs`, `estimate`.

## Segments too large for one job

A segment whose estimate exceeds `work_cap` cannot run as is. The scheduler hands it to `too_large` with `work_cap` as its whole budget, and `refine_large` samples the segment again, inside the segment only, at the lowest stratum whose samples fit. Lines unique among those samples on both sides are chained as the skeleton chains anchors, and the chain cuts the segment into pieces. A piece that fits is refined. A piece with the same line count on both sides is compared byte by byte and, when equal, kept as one equal run. Any other piece is sampled again, at a lower stratum still.

Only a piece that is not equal and has no anchor at any lower stratum becomes a whole replacement, and its hunk is marked approximate. These cuts need no confirmation: each piece's own script is exact, so any cut gives a valid script.

Sources: `crates/rift-core/src/diff/resample.rs`, `refine_large`, `split`, `chain`, `whole`.

## When a segment comes out approximate

Two cases produce `approx`: a piece that passes the Myers cost cutoff, and a too-large piece with no anchor below its stratum. The second needs repetitive data. The test `approximate_regions_are_marked_and_still_patch` builds one: 300,000 copies of `same line` with one byte changed in the middle. No line is unique at any stratum.

The same pair from the command line:

| `--mem` | Unified diff | Statistics |
|---|---|---|
| 16M | `@@ -1,300000 +1,300000 @@ approx`, 600,003 lines | approx 1 segments, 600000 lines in a and b |
| 64M | the same | the same |
| 256M | `@@ -149998,7 +149998,7 @@`, 11 lines | approx 0 segments |

At 256M the one segment of 600,000 lines fits a job and Myers finds the single change. Below that it is too large, resampling finds no anchor, and Rift prints the whole region as one replacement with the mark. Either diff patches A into B. Measured 2026-11-07 with `rift --mem M --stats ra rb` on an Apple M1, 16 GiB, macOS 27.0.1.

The mark appears in every output: `approx` after the hunk header in the unified diff, a count in `--summary` and `--stats`, and a marker in the terminal view.

Sources: `crates/rift-cli/tests/full.rs`, `approximate_regions_are_marked_and_still_patch`; `crates/rift-core/src/unified/mod.rs`; `diff/mod.rs`, `Written`.
