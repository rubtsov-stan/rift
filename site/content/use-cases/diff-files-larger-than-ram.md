+++
title = "Diff files larger than RAM without running out of memory"
description = "How to diff files that do not fit in memory: rift keeps its heap under --mem. Measured on pairs up to 10 GiB, with the numbers and where they come from."
weight = 3
[extra]
nav = "Files larger than RAM"
h1 = "Diff files larger than RAM"
lead = "GNU diff holds both files in memory. On a big pair it gets killed, or the machine starts to swap."
summary = "Set a heap limit and diff a pair of any size you have measured, with the numbers."
+++

Rift reads the files in fixed buffers and keeps a sample of their lines. Its heap stays at or under the limit you set with `--mem`.

## What a small limit does

A 1 GiB pair of generated SQL dumps, with the heap limit at 64 MiB:

{{ term(name="big-summary", kind="summary", title="a 1 GiB pair at --mem 64M") }}

The summary has the same shape as for a small pair. The sample is thinner, so fewer lines serve as anchors and more segments need refining.

## What was measured

The largest pair measured so far is 50 GiB, on an Intel i5-10600 with 16 GB of RAM, read from a SATA SSD: Rift wrote the full diff, 1.2 GiB of output, in 7.6 min with a heap of 80.6 MiB. On a Mac with 16 GB of RAM, `diff` and `git diff` were not run on a 10 GiB pair, because 20 GiB of input is more than the RAM; Rift finished the full diff in 25.48 s with a heap of 92.4 MiB and a resident set of 132.9 MiB. The tables and the commands are on the [benchmarks page](@/benchmarks.md). No pair above 50 GiB has been measured.

| Pair | Tool | Time | Peak memory |
|---|---|---|---|
| 1 GiB | `rift` (full diff) | 0.91 s | 127.2 MiB |
| 1 GiB | system `diff` | 17.95 s | 3.1 GiB |
| 10 GiB | `rift` (full diff) | 25.48 s | 132.9 MiB |
| 10 GiB | `diff` | not run: 20 GiB of input, 16 GB of RAM | |

Apple M1, 16 GB RAM, internal SSD, macOS 27.0.1, page cache not dropped, one run of each, Rift at `e06f3d4`, 2026-11-15. Source: `bench/results/macos-apple-m1-2026-11-15/report.md`.

## When the limit is too small

If the budget is below what the pair needs, Rift exits with status 2 and prints a limit to rerun with. The advice can name a higher limit again on the next run: see [the memory guide](@/guide/memory.md).

## What the limit does not cover

Only the heap is bounded. The resident set also holds the binary, thread stacks and the page cache the kernel counts against the process.
