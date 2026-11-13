+++
title = "--mem and \"rerun with --mem N\": the heap limit"
description = "How --mem limits Rift's heap, what the budget error means, why the suggested limit can fail again, and what the resident set holds on top."
weight = 6
[extra]
nav = "--mem"
h1 = "--mem and the budget error"
lead = "The heap limit is the one promise Rift keeps for you: its heap stays at or under --mem."
summary = "Set the heap limit, read the rerun advice, and know what the limit does not cover."
+++

```
rift --mem 1G a.csv b.csv
```

`--mem` takes bytes or a number with a `K`, `M`, `G` or `T` unit, in powers of 1024. The default is `256M` and the smallest is `64K`.

Rift sizes its work for one thread per MiB of the limit, from 1 to 8, in each of the scan, the refinement and its thread pool, and starts no more threads than the machine has cores; `RAYON_NUM_THREADS` can make the pool smaller. It keeps 48 KiB of the limit, plus 12 KiB for each thread the limit allows, for heap that no budget counts: the command line, output buffers, messages and the thread pool. Key mode keeps only the 48 KiB: it runs on threads of its own, whose heap it takes from the budget. The rest is the budget. Rift reserves every allocation on the data path from it before it makes it, and reads files with `pread` into fixed buffers. It does not map files into memory. The tests and CI check the peak of the whole process with a counting allocator.

The output is identical on any machine for the same `--mem` and input. The cores and `RAYON_NUM_THREADS` change how fast Rift runs, not what it prints.

## A larger file does not need a larger limit

On a 1 GiB pair of generated SQL dumps, with the limit at 64 MiB, Rift counted 13 million lines and finished:

{{ term(name="big-summary", kind="summary", title="a 1 GiB pair at --mem 64M") }}

A smaller budget means a thinner sample of lines, so more segments are coarse and more work happens when a region is refined. A larger budget keeps more detail. In the benchmark runs on a 1 GiB pair, `--mem 64M` peaked at 21.0 MiB of heap and `--mem 1G` at 329.2 MiB, against 83.6 MiB at the default.

## When the limit is too small

Below the smallest limit that works for a pair, Rift exits with status 2 and a budget error. The error ends with a limit to rerun with, the reserve included:

{{ term(name="mem-error", title="a limit that is too small") }}

The smallest limit that works depends on the files, with one fixed floor: a full diff needs about 10.3 MiB (`10475K`) even for two files of a few bytes, because the buffers that compare the lines of a region are about 5 MiB, and the refinement gets half the limit. `--summary` and key mode do not compare regions and run below it. At exactly `10475K` a region may still come out approximate; `11M` is exact for small files.

Each limit a refusal names is at least twice the one that was refused, so a few reruns reach one that works. The last step can overshoot what the run needs by up to a factor of two. The limit in the message is not a lower bound for the whole run. A run at that limit can stop at a later stage and name a higher one:

{{ term(name="mem-rerun", title="the suggested limit, run again") }}

So take the advice as a direction. Follow it, or start from a limit with room, such as `2M` here:

{{ term(name="mem-ok", kind="summary", title="a limit with room") }}

A run that finishes can still mark regions approximate for lack of budget. The plain diff says so only in the hunk header; `--stats` also prints a `--mem` to try, twice the one used, on the line that counts those regions.

## What the limit does not cover

Only the heap is bounded by `--mem`. The resident set also holds the binary, thread stacks and whatever page cache the kernel counts against the process. On the 1 GiB pair the resident set peaked at 113 MiB with the heap at 83.6 MiB, both at the default limit: see the [benchmarks](@/benchmarks.md).

In [key mode](@/guide/key-mode.md) a larger limit means fewer passes over the files.
