+++
title = "How to diff two large SQL dumps"
description = "Compare two pg_dump or SQL files of many gigabytes with rift: a summary first, then a unified diff that patch applies. Real output on a 64 MiB pair."
weight = 1
[extra]
nav = "Diff two SQL dumps"
h1 = "How to diff two large SQL dumps"
lead = "You dumped a database before and after a migration and want to know what changed. Plain diff reads both files into memory. Rift does not."
summary = "Summary first, then the unified diff, on a pair of generated dumps."
+++

## 1. Count the differences

Start with `--summary`. It writes no diff and tells you how much differs:

{{ term(name="sql-summary", kind="summary", title="a.sql, b.sql: a 64 MiB pair") }}

Here 435 segments differ out of 871, and 8,001 lines of the first file are involved. The peak heap was 78.6 MiB against the default limit of 256 MiB.

## 2. Write the diff

```
rift a.sql b.sql > changes.patch
```

{{ term(name="sql-diff-excerpt", kind="diff", title="the first and the third hunk", caption="Timestamps dropped, lines cut at 70 columns.") }}

The patch is a unified diff. `patch a.sql changes.patch` turns the first dump into the second, byte for byte.

## 3. If the dump is a pg_dump with COPY blocks

Row order in a `pg_dump` can change between runs without any row changing. If it does, compare by key instead of by line: [key mode](@/guide/key-mode.md) reads the `COPY` blocks with `--key copy:id`.

## Memory

The default limit is 256 MiB. On a 1 GiB pair the heap peaked at 83.6 MiB, and the resident set at 113 MiB, in the [benchmarks](@/benchmarks.md). Raise the limit with `--mem` if you want a finer diff: see [the memory guide](@/guide/memory.md).
