+++
title = "Anchors and the skeleton: patience chain, lazy checks"
description = "How Rift picks lines unique in both files, chains them with a patience LIS, confirms anchors byte by byte as it compares gaps, and demotes false ones."
weight = 2
[extra]
nav = "Anchors and the skeleton"
h1 = "Anchors and the skeleton"
lead = "The skeleton is the segment map of the whole pair: where the files agree, confirmed byte by byte, and where they differ. It costs two passes over the disk and memory in proportion to the sample."
summary = "Unique lines, the longest chain, lazy byte checks, false anchors and shingle anchors."
+++

[DESIGN.md](https://github.com/rubtsov-stan/rift/blob/main/DESIGN.md) covers this stage under "Anchors and skeleton" and "Shingle anchors".

## Candidates: lines unique in both files

After the scan, Rift holds a sample of each file at one stratum. A sampled line whose hash occurs once among A's samples and once among B's is a candidate anchor, the same idea as patience diff applied to a sample.

`UniqueLines` counts occurrences in an open-addressing table keyed by the line hash, with linear probing and 16-byte entries. Only the file with fewer samples goes into the table; the other file probes it, since a hash missing from the smaller side cannot be an anchor anyway. Each entry records the sample index on each side, or a marker that the hash occurred more than once there. The table is sized from the budget before it is filled and kept at most 7/8 full. When it does not fit the skeleton half, the build raises the stratum and tries again.

A candidate is a hash match. Nothing has compared its bytes yet.

Sources: `crates/rift-core/src/anchors/mod.rs`, `UniqueLines`, `unique_hashes`, `MAX_LOAD`; `crates/rift-core/src/skeleton/mod.rs`, `build_watched`.

## The longest chain

The candidates, ordered by their line in A, are reduced to the longest chain that also increases in B. `longest_increasing` uses patience sorting: one pass over the candidates with a binary search per candidate, O(m log m) time for m candidates and 12 bytes each of working memory. Files that mostly agree put most candidates at the end of the longest chain so far, and the search checks that case first and skips the binary search.

Candidates left out of the chain cross it. A block of rows that moved produces such candidates, so the skeleton keeps them as the off-chain list, 16 bytes per pair, capped at an eighth of the skeleton half that is free when the search starts. [Moved blocks](@/design/moves.md) starts from that list.

Each anchor in the chain opens a gap that runs to the next anchor. Gap 0 starts at the top of both files, the last gap ends at their ends, and every line of both files lies in exactly one gap.

Sources: `skeleton/mod.rs`, `longest_increasing`, `OffChain`, `Bounds`.

## The second pass: comparing gaps

A gap with the same line count and the same byte length on both sides may be equal. `compare_gaps` streams such gaps through two buffers and compares them byte by byte. Consecutive gaps of that shape are read as one range, so a long identical stretch costs one sequential read of each file, whatever the number of anchors in it. A mismatch names its gap by a binary search on the byte position, and the stream restarts after it.

Rift spreads the work over the rayon pool: each task owns the gaps that start in 16 MiB of A, and tasks go round robin to a few lanes, each with its own pair of buffers of at most 1 MiB. One thread that alternates between two files reads at a fraction of the disk's speed, which is why the comparison runs in parallel.

Gaps of different shape are not read: they differ, and the scheduler refines them later. Equal neighbours merge into one `Equal` segment.

Sources: `skeleton/mod.rs`, `compare_gaps`, `first_mismatch`, `build_at`, `TASK_BYTES`, `MAX_BUF`.

## Lazy verification and false anchors

<figure class="diagram">
<svg viewBox="0 0 720 320" role="img" aria-labelledby="anc-t anc-d">
<title id="anc-t">Anchors cut the files into gaps; a false anchor is demoted</title>
<desc id="anc-d">Two bars for files A and B, joined by four anchors in the same order and one dashed candidate that crosses them and is left out of the chain. The anchors cut five gaps. Gap 0 and gap 4 have the same shape on both sides and compare equal. Gap 1 has different lengths and differs. The third anchor turns out to have different bytes on the two sides: it was a hash collision. Rift demotes it, and gap 2 and gap 3 merge into one differing segment. The resulting segments are equal, differs, differs, equal.</desc>
<text x="20" y="22" font-weight="700">A</text>
<rect class="box" x="20" y="30" width="680" height="24"/>
<text x="20" y="152" font-weight="700">B</text>
<rect class="box" x="20" y="160" width="680" height="24"/>
<path class="link" d="M150 54 L150 160 M300 54 L330 160 M560 54 L560 160"/>
<path d="M420 54 L440 160" style="stroke:var(--red);stroke-width:2;fill:none"/>
<path d="M412 98 L432 118 M432 98 L412 118" style="stroke:var(--red);stroke-width:2.5"/>
<path class="link" stroke-dasharray="5 4" d="M230 54 L520 160"/>
<text x="74" y="76" style="font-size:12px">gap 0</text><text x="200" y="76" style="font-size:12px">gap 1</text><text x="340" y="76" style="font-size:12px">gap 2</text><text x="470" y="76" style="font-size:12px">gap 3</text><text x="610" y="76" style="font-size:12px">gap 4</text>
<rect class="eq" x="20" y="222" width="130" height="24"/><rect class="df" x="150" y="222" width="150" height="24"/><rect class="df" x="303" y="222" width="257" height="24"/><rect class="eq" x="560" y="222" width="140" height="24"/>
<text x="20" y="214" style="font-size:12px">segments, in A</text>
<text x="20" y="270" style="font-size:12px">equal</text><text x="150" y="270" style="font-size:12px">differs</text><text x="303" y="270" style="font-size:12px">differs: gaps 2 and 3 merged</text><text x="610" y="270" style="font-size:12px">equal</text>
<text x="20" y="292" style="font-size:12px">green: compared byte by byte and equal; red: differs, refined on demand</text>
<text x="20" y="312" style="font-size:12px">dashed: off-chain candidate; red line with a cross: false anchor, demoted</text>
</svg>
<figcaption>Each anchor opens the gap after it. Gap 3 differs, so Rift reads its anchor's two lines, finds them unequal, and merges gap 2 into the same differing segment.</figcaption>
</figure>

Every anchor in the chain is confirmed byte by byte, but none is read on its own when it does not need to be. The anchor's line is the first line of the gap it opens. When that gap is compared and found equal, the anchor's bytes were part of the comparison. When the gap differs, the task that owns the gap reads the anchor's two lines with `same_line`, starting with 256 bytes from each file and continuing in buffer-sized steps for a longer line.

An anchor whose two lines differ is a false anchor: two different lines with the same 64-bit hash. Rift demotes it. The gap it closes is marked as differing and merges with the gap it opens into one `Differs` segment, and the anchor drops out of the count that `--summary` prints. A false anchor can only make the map coarser. No line becomes `Equal` without a byte comparison.

The test `a_false_anchor_is_demoted_and_never_equal` forces the case with a source whose line 200 reads differently after the scan than during it. At stratum 0 every one of the 400 lines is an anchor; the map comes out with one differing segment, lines 199 and 200 (counted from 0) on both sides, and 399 anchors.

Confirming every candidate before the chain is built would be simpler and slower. DESIGN.md records why: on the 1 GiB generated pair (seed 7, Apple M1, warm cache), confirming candidates up front took 1.6 s of a 2.3 s run. Reading the anchors of differing gaps after the comparison, on one thread, took the 10 GiB pair from 13 s to between 23 and 27 s with a cold cache. Inside the comparison tasks, those reads run in parallel and next to data the task reads anyway.

Sources: `skeleton/mod.rs`, `compare_gaps` (the `differs` closure), `same_line`, `push`, `Skeleton::anchors`; `crates/rift-core/tests/skeleton.rs`, `a_false_anchor_is_demoted_and_never_equal`; DESIGN.md, "Anchors and skeleton".

## Shingle anchors for data without unique lines

A table dump without an id column can hold almost no unique lines. The generator's `nokey` profile draws every row from a pool of 1024, so in a file of a few MiB no line occurs only once, and unique lines give the chain nothing to work with.

For those stretches Rift adds a second source of candidates, `Shingles`. A shingle is a window of three consecutive samples. Its hash combines the three line hashes with the line count and the byte length from each sample to the next, rolled as a polynomial from one window to the next, so nothing is read again. A window whose hash occurs once in each file becomes a candidate that points at its first sample. The rest of the window lies in the gaps after it, which are compared or refined like any other.

`anchors::select` looks for shingles only in stretches of at least 32 samples between unique-line candidates, on either side. On data with an id column those stretches hold under 1% of the samples, and the shingle search costs one walk over the candidates. Shingles that clash with a unique-line candidate are dropped. When the shingles or the merge do not fit the skeleton half, the unique lines are used alone and the build does not fail.

On a 64 MiB `nokey` CSV pair (`rift-gen --seed 7 --size 64M --profile nokey --rows csv`, 4,602,404 lines in A), `rift --summary --mem 256M` built its map at stratum 2 with 1,010,278 anchors and no approximate segment, in 0.21 s. DESIGN.md has the measurements that chose the window of three samples and the 32-sample rule. Measured 2026-11-07, Apple M1, 16 GiB, macOS 27.0.1, warm page cache.

Sources: `anchors/mod.rs`, `select`, `add_shingles`, `merge`, `SHINGLE_W`, `SHINGLE_MIN_GAP`; `crates/rift-core/src/anchors/shingles.rs`, `Shingles::in_gaps`, `Windows`; `crates/rift-gen/src/profile.rs`, `NOKEY_POOL`.
