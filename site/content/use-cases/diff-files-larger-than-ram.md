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

The largest pair measured so far is 10 GiB. On a Mac with 16 GB of RAM, GNU `diff` and `git diff` did not run on it, because they need more than 16 GB. Rift finished the full diff in 26.16 s with a heap of 94.1 MiB and a resident set of 136.5 MiB. The table and the commands are on the [benchmarks page](@/benchmarks.md). No pair above 10 GiB has been measured.

| Pair | Tool | Time | Peak memory |
|---|---|---|---|
| 1 GiB | `rift` (full diff) | 0.93 s | 130.8 MiB |
| 1 GiB | system `diff` | 17.55 s | 3.2 GiB |
| 10 GiB | `rift` (full diff) | 26.16 s | 136.5 MiB |
| 10 GiB | `diff` | did not run: needs more than 16 GB of RAM | |

Apple M1, 16 GB RAM, internal SSD, macOS, warm page cache. Source: the Benchmarks section of `README.md`.

## When the limit is too small

If the budget is below what the pair needs, Rift exits with status 2 and prints a limit to rerun with. The advice can name a higher limit again on the next run: see [the memory guide](@/guide/memory.md).

## What the limit does not cover

Only the heap is bounded. The resident set also holds the binary, thread stacks and the page cache the kernel counts against the process.
