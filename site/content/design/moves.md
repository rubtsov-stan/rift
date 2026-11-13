+++
title = "Moved blocks: found from off-chain anchors, partial recall"
description = "How Rift finds blocks of lines that moved, why it misses some at coarse strata, and why listing moves leaves the unified diff byte for byte the same."
weight = 6
[extra]
nav = "Moved blocks"
h1 = "Moved blocks"
lead = "A block of rows that moved shows up in a line diff as a deletion in one place and an insertion in another. Rift lists such blocks next to the diff, as a note on it."
summary = "How moved blocks are found, why recall is partial, and why the unified diff does not change."
+++

[DESIGN.md](https://github.com/rubtsov-stan/rift/blob/main/DESIGN.md) covers this under "Moved blocks". The [moves guide](@/guide/moves.md) shows the output.

## Seeds: the candidates the chain left out

<figure class="diagram">
<svg viewBox="0 0 720 272" role="img" aria-labelledby="mv-t mv-d">
<title id="mv-t">A moved block and the seed that finds it</title>
<desc id="mv-d">Files A and B as two bars. A block in A appears later in B. The chain of anchors runs straight down on both sides of it. One sampled line inside the block is unique in both files, but its pair crosses the chain, so the chain leaves it out. Rift takes that pair as a seed, compares the two lines, and extends the run up and down while the lines stay equal. The unified diff still deletes the block in A and inserts it in B.</desc>
<text x="20" y="22" font-weight="700">A</text>
<rect class="box" x="20" y="30" width="680" height="24"/>
<rect class="df" x="180" y="30" width="110" height="24"/>
<text x="20" y="170" font-weight="700">B</text>
<rect class="box" x="20" y="178" width="680" height="24"/>
<rect class="eq" x="470" y="178" width="110" height="24"/>
<path class="link" d="M100 54 L100 178 M160 54 L160 178 M330 54 L310 178 M640 54 L640 178"/>
<path stroke-dasharray="5 4" d="M230 54 L520 178" style="stroke:var(--accent);stroke-width:2.5;fill:none"/>
<text x="400" y="96" style="font-size:12px">off-chain pair: the seed</text>
<path d="M180 64 H290 M470 168 H580" style="stroke:var(--accent);stroke-width:2;fill:none"/>
<text x="20" y="232" style="font-size:12px">unified diff: lines deleted in A (red) and inserted in B (green), as before;</text>
<text x="20" y="248" style="font-size:12px">the block list adds the pair of ranges; the short rules: the run, extended both</text>
<text x="20" y="264" style="font-size:12px">ways from the seed</text>
</svg>
<figcaption>Solid lines are anchors on the chain. The dashed pair is a hash match between sampled lines that crosses the chain, the kind a moved block leaves behind.</figcaption>
</figure>

A moved block keeps its lines. Some of them were sampled and are unique in both files, so they are anchor candidates, and their pairs point from the block's old place in A to its new place in B. Those pairs cross the chain the skeleton keeps, so the chain search leaves them out. The skeleton keeps them anyway, as the off-chain list, because a moved block looks like that from the sample.

`Moves::find` takes each pair as a seed, in A order. A pair is a hash match, so it first compares the two lines byte by byte. It then extends the run forward and backward, reading 16 lines per side at a time, while the lines on both sides are equal and neither lies in an `Equal` segment. Runs of at least 3 lines are kept. A seed inside a block already found adds nothing, so consecutive seeds of one block make one run, and a seed costs two reads.

Every block reported is exact: two line ranges of the same length that hold the same bytes. The minimum of 3 lines filters out a changed row that happens to equal some other row, together with one neighbour.

Sources: `crates/rift-core/src/moves/mod.rs`, `Moves::find`, `Run::extend`, `MIN_LINES`, `READ_LINES`; `crates/rift-core/src/skeleton/mod.rs`, `OffChain`, `longest_increasing`.

## Why recall is partial

A block is found only if one of its lines was sampled. At stratum `L` each line is sampled with probability `2^-L`, so a block of `n` lines has no seed, and goes unreported, with probability about `(1 - 2^-L)^n`. A short block at a coarse stratum is likely to slip through. The off-chain list has a cap too, an eighth of the skeleton half free when the chain search starts, and `--summary` prints how many candidates the skeleton dropped when it dropped any.

The generator's manifest makes this measurable. On a 256 MiB pair from `rift-gen --seed 7 --size 256M --profile shuffled --rows csv`, the manifest lists 6,638 moves of 3 lines or more, 177,904 lines in all. `rift --summary` found:

| `--mem` | Stratum | Blocks found | Share | Expected from `(1 - 2^-L)^n` |
|---|---|---|---|---|
| 1G | 0 | 6,638 (177,904 lines) | 100% | 6,638 |
| 256M | 2 | 6,435 | 96.9% | 6,423 |
| 64M | 4 | 4,897 | 73.8% | 4,934 |

The expected column sums `1 - (1 - 2^-L)^n` over the manifest's moves. At stratum 0 the found blocks cover the manifest's 177,904 lines, no more and no fewer. Measured 2026-11-07 on an Apple M1, 16 GiB, macOS 27.0.1; the expected values come from an `awk` script over `manifest.tsv`.

The integration tests check the other direction, that nothing false comes out. On 2 MiB generated pairs, where every line is sampled, `every_shuffled_block_move_is_found` and `moves_in_the_other_profiles_are_found` require every manifest move of 3 lines or more and nothing else, and `pairs_without_moves_report_none` requires an empty list on pairs generated without moves.

Sources: `moves/mod.rs`, module docs; `crates/rift-cli/tests/moves.rs`.

## Why the unified diff does not change

A moved block annotates the diff. The edit script still deletes the block where it was and inserts it where it is, and the unified diff prints it that way, so `patch` and every tool that reads diffs see what they always saw. No line is in two blocks: a block that would share lines of B with another loses them, and is dropped when fewer than 3 lines remain. The list lives beside the script: `--summary` and `--stats` print the block count and the first ten blocks, and the terminal view marks both ends of a block with `m` and lets you jump from one end to the other.

The test `no_moves_leaves_the_unified_diff_unchanged` runs five generated pairs at `--mem 16M` and `256M`, with and without `--no-moves`, and requires the same bytes on standard output. On the 256 MiB pair above, the two diffs had the same MD5 at `--mem 64M`, and again at `--mem 256M`.

Sources: `crates/rift-core/src/diff/mod.rs`, `Config` (`moves`); `crates/rift-cli/tests/moves.rs`, `no_moves_leaves_the_unified_diff_unchanged`; `crates/rift-core/src/diff/tests.rs`, `moves_off_finds_nothing_and_keeps_the_script`.

## What it costs

Moved blocks are paid from the refine half. The list takes 32 bytes per block, for at most one block per seed and at most an eighth of the half's capacity. The diff reserves it before the workers and the scheduler and holds it for its whole life, with the search on or off, so the search never changes what the scheduler gets, and with it which regions come out approximate. While the search runs, it also takes two readers with 4 KiB buffers and two comparison buffers of up to 128 KiB each. When those or the list do not fit, the search keeps nothing, and `--summary` says so:

```
moved    not searched, the budget is too small
```

That line is from the 2,000-line pair of the [memory budget](@/design/memory.md) page at `--mem 304K`. When the list fills up, the search stops and keeps the blocks it found, and the moved line ends with `the list is full at this --mem, later moves are not listed`. `--no-moves` skips the search and allocates nothing for it, though the diff still sets the list's share aside; `--summary`, `--stats` and the terminal status line say that moves are off.

Sources: `moves/mod.rs`, `Moves::find`, `Moves::list_bytes`, `work`, `BLOCK_SHARE`, `Moves::skipped`, `Moves::full`; `diff/mod.rs`, `Diff::with_watch`.
