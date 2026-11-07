+++
title = "Plain diff: a unified diff of two large files"
description = "Run rift a b to write a unified diff that patch applies. Same format as diff -u, with a heap limit you set and a stats line on request."
weight = 1
[extra]
nav = "Plain diff"
h1 = "Plain diff"
lead = "Give Rift two files and it writes a unified diff to standard output, the same format as diff -u."
summary = "Write a unified diff that patch applies."
+++

```
rift a.sql b.sql > changes.patch
```

The heap Rift holds never passes the limit you set with [`--mem`](@/guide/memory.md). The default is 256 MiB.

Here is the start of a diff of two generated SQL dumps. The files are 64 MiB each, and a change starts at about one line in a thousand.

{{ term(name="sql-diff-excerpt", kind="diff", title="the first and the third hunk of 434", caption="Timestamps dropped from the two header lines, and each line cut at 70 columns.") }}

## Apply the diff

The diff is an edit script. Applied to the first file with `patch`, it gives the second file byte for byte, line endings and a missing final newline included.

{{ term(name="sql-patch", title="patch round trip") }}

## Peak heap and time

`--stats` prints the peak heap, the time, the approximate regions and the moved blocks to standard error, so the diff on standard output stays clean:

{{ term(name="sql-stats", title="rift --stats") }}

Rift marks a region it could not refine within the budget as approximate: the unified diff puts the word `approx` after the closing `@@` of the hunk header, where `diff -p` puts a function name, and `patch` ignores it. Here the count is 0.

## What it does not promise

The diff is not minimal. Like patience diff in git, Rift can show a change as larger than the smallest possible one. Correctness is the promise: the output always rebuilds the second file from the first.
