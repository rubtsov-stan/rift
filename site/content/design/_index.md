+++
title = "Rift design: fixed memory, time that grows with the files"
description = "How Rift turns two large files into a diff: a sampled map built in two passes, refinement on demand, and a heap limit that never moves."
template = "section.html"
page_template = "page.html"
sort_by = "weight"
aliases = ["how-it-works/"]
[extra]
nav_title = "Design"
h1 = "Design"
lead = "Rift fixes its memory before it reads a byte and lets the running time grow with the files. These pages explain each stage of that trade and point to the code that makes it."
+++

The reference for the algorithm and its invariants is [DESIGN.md](https://github.com/rubtsov-stan/rift/blob/main/DESIGN.md) in the repository. These pages follow the code as it stands today, and where the two disagree, they follow the code.

## The one rule

You give Rift a heap limit with `--mem` (256 MiB by default). Rift splits it into two halves and never asks for more. A larger file costs more time and more reads, and the same memory.

The benchmark run on Linux shows the rule from both sides. On generated SQL dumps at `--mem 256M`, the full diff of a 1 GiB pair peaked at 83.6 MiB of heap and took 0.69 s; the 2 GiB pair peaked at the same 83.6 MiB and took 1.2 s. On an Apple M1 the 10 GiB pair took 26.16 s with a resident set of 136.5 MiB, against 0.93 s and 130.8 MiB for 1 GiB. On the 1 GiB pair the system diff needed 3.2 GiB on the Mac, and GNU diff was killed by a 3 GiB memory cap on Linux.

Sources: `bench/results/linux-intel-core-i5-10600-2026-11-06/report.md`, runs 025, 083 and 030; `site/data/bench-mac.json`, from the README benchmark table.

The budget also changes the result, within limits. A larger `--mem` buys a finer map and a smaller diff, and costs time: on the same 1 GiB Linux pair, `--mem 64M` peaked at 21.0 MiB of heap in 0.54 s and `--mem 1G` at 329.2 MiB in 1.2 s (runs 028 and 029). Both outputs are correct edit scripts.

## From two files to a diff

<figure class="diagram">
<svg viewBox="0 0 720 306" role="img" aria-labelledby="pipe-t pipe-d">
<title id="pipe-t">Rift's pipeline from two files to a diff</title>
<desc id="pipe-d">Four stages in a row. Stage 1 scans both files with pread, hashes every line with xxh3 and keeps a sample at stratum L. Stage 2 picks lines unique in both files and keeps the longest chain in order. Stage 3 compares the gaps between anchors byte by byte and marks each segment equal or differs. Stages 1 and 2 read the files once, stage 3 a second time, and all three are paid from the skeleton half of the budget. Stage 4 refines differing segments on demand with patience and Myers diff, paid from the refine half. The segment map feeds the terminal view, the unified diff and the summary.</desc>
<defs><marker id="pipe-arrow" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="8" markerHeight="8" orient="auto"><path d="M0 0 L8 4 L0 8 Z" style="fill:var(--muted)"/></marker></defs>
<text x="10" y="18" style="font-size:12px">pass 1: read A and B</text>
<text x="370" y="18" style="font-size:12px">pass 2: read what may be equal</text>
<rect class="box" x="10" y="30" width="160" height="78"/><rect class="box" x="190" y="30" width="160" height="78"/><rect class="box" x="370" y="30" width="160" height="78"/><rect class="box" x="550" y="30" width="160" height="78" style="stroke:var(--accent)"/>
<text x="20" y="52" font-weight="700">1 scan</text><text x="20" y="74" style="font-size:12px">pread, xxh3</text><text x="20" y="94" style="font-size:12px">sample, stratum L</text>
<text x="200" y="52" font-weight="700">2 anchors</text><text x="200" y="74" style="font-size:12px">unique in A and B</text><text x="200" y="94" style="font-size:12px">longest chain</text>
<text x="380" y="52" font-weight="700">3 compare gaps</text><text x="380" y="74" style="font-size:12px">byte by byte</text><text x="380" y="94" style="font-size:12px">equal or differs</text>
<text x="560" y="52" font-weight="700">4 refine</text><text x="560" y="74" style="font-size:12px">on demand:</text><text x="560" y="94" style="font-size:12px">patience, Myers</text>
<path class="link" d="M170 69 H188" marker-end="url(#pipe-arrow)"/><path class="link" d="M350 69 H368" marker-end="url(#pipe-arrow)"/><path class="link" d="M530 69 H548" marker-end="url(#pipe-arrow)"/>
<path class="link" d="M10 126 V134 H530 V126 M550 126 V134 H710 V126"/>
<text x="10" y="154" style="font-size:12px">skeleton half, B/2: samples, index, anchor table, map</text>
<text x="550" y="154" style="font-size:12px">refine half, B/2</text>
<rect class="eq" x="10" y="182" width="120" height="22"/><rect class="df" x="130" y="182" width="40" height="22"/><rect class="eq" x="170" y="182" width="200" height="22"/><rect class="df" x="370" y="182" width="30" height="22"/><rect class="eq" x="400" y="182" width="310" height="22"/>
<text x="10" y="226" style="font-size:12px">segment map: equal (green) and differs (red), tiling both files</text>
<path class="link" d="M630 108 V176" marker-end="url(#pipe-arrow)"/>
<text x="10" y="262" font-weight="700">outputs</text>
<text x="10" y="282" style="font-size:12px">terminal view: refines the hunks on screen; unified diff: every segment;</text><text x="10" y="298" style="font-size:12px">--summary: the map and the moved blocks, no refinement</text>
</svg>
<figcaption>Stages 1 to 3 build the map in two passes over the disk and are paid from one half of the budget. Stage 4 runs only where an output needs a line-level diff, from the other half.</figcaption>
</figure>

**Scan.** Rift reads each file through `pread` into fixed buffers, finds line ends with `memchr` and hashes every line with xxh3. It keeps a sample: the lines whose hash has its low `L` bits at zero, plus a sparse index of line starts. Both files pick the same lines, because the choice depends on the line's bytes alone. Scanning covers the details and how `L` follows from the budget.

**Anchors.** A sampled line that occurs once in each file is a candidate anchor. The longest chain of candidates in the same order on both sides cuts the pair into gaps, as patience diff does. Where no line is unique, windows of three samples serve as anchors instead. See Anchors and the skeleton.

**Compare gaps.** A gap with the same line count and byte length on both sides is streamed and compared byte by byte. A run of such gaps that match becomes one `Equal` segment; anything else is `Differs`. This is the second pass over the disk, and it confirms the anchors as a side effect.

**Refine.** A differing segment gets a line-level diff only when an output asks for it: the terminal view for the hunks on screen, the unified diff writer for each segment in turn. A scheduler runs these jobs within the refine half and caches the results. A segment too large for that half is sampled again inside itself; if even that fails, it becomes one replacement marked approximate. See Refinement.

Sources: `crates/rift-core/src/diff/mod.rs`, `Diff::with_watch`; `skeleton/mod.rs`, `build_pair_watched` and `build_at`; `sched/mod.rs`, `Scheduler::new` and `worker`.

<!-- Verified: Diff::with_watch calls build_pair_watched (scan A, scan B, build), then into_index drops the samples, then reserves the front end, workers, moves and the scheduler from Half::Refine. -->

## What you can rely on

The output is a correct edit script: applied to A, it yields B. A line is reported equal only after Rift compared its bytes; a matching hash is never enough. Regions that Rift could not diff line by line within the budget carry an `approx` mark in the unified diff, the terminal view and the statistics. Correctness lists the tests and fuzz targets behind each of these claims.

You give up two things. The diff is not minimal, as with patience diff in git, and it can change with `--mem`: on a 256 MiB pair of the generator's `shuffled` profile, the diff at `--mem 64M` has 604,516 lines and the one at `--mem 256M` 603,585, and each patches A into B (checked with `gpatch` on 2026-11-07, Apple M1). A moved block shows as a deletion and an insertion in the unified diff; Rift lists moved blocks separately, and finds most of them, as Moved blocks measures.

