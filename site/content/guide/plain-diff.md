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

Rift marks a region it could not align within the budget as approximate: the unified diff puts `approx:` and the reason after the closing `@@` of the hunk header, where `diff -p` puts a function name, and `patch` ignores it. Here the count is 0. An approximate hunk is still correct, only coarser than it could be. The reasons:

| Reason in the header | What it means |
|---|---|
| `too many changes among repeated lines to align them exactly` | No line in the region occurs once in each file, and the search for the smallest diff hit its cost limit. |
| `found no line here that occurs once in each file, so there is nothing to align on` | The region was too large to diff in one piece, and Rift found no line to cut it at. |
| `too large to align within --mem; a larger --mem may help` | The region ran out of its share of the budget. |

`--stats` counts the approximate segments for each reason. Two files that share no line come out as one deletion and one insertion without the mark: with nothing in common, that is the smallest diff.

## What it does not promise

The diff is not minimal. Like patience diff in git, Rift can show a change as larger than the smallest possible one. Correctness is the promise: the output always rebuilds the second file from the first.
