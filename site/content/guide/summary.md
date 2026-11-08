+++
title = "rift --summary: how much differs, without the diff"
description = "Print line counts, the segment map, the first differing ranges, moved blocks, peak heap and time for two huge files, without writing a diff."
weight = 2
[extra]
nav = "--summary"
h1 = "--summary"
lead = "The summary tells you how much differs between two files before you write the diff."
summary = "Line counts, segments, the first differing ranges, moved blocks and resource use."
+++

```
rift --summary a.sql b.sql
```

{{ term(name="sql-summary", kind="summary", title="a.sql, b.sql: a 64 MiB pair") }}

Read it from the top:

- **Counts.** The line and byte count of each file.
- **Segments.** Rift cuts the pair into segments at lines that occur once in each file. `equal` segments have identical bytes on both sides, checked byte by byte. `differs` segments hold the changes. The summary does not diff them line by line, so it cannot tell which ones would come out approximate; run the full diff with `--stats` for that count.
- **Ranges.** The first 10 differing ranges, counted from 0, end excluded. `a 381..383  b 381..382` means lines 381 and 382 of the first file became line 381 of the second.
- **Moved blocks.** Blocks of at least three lines that appear in both files at different places. See [moved blocks](@/guide/moves.md).
- **Resource use.** The peak heap against the limit, and the time.

The summary writes no diff, so it is the cheapest run. On a 1 GiB pair it finished in 0.51 s on the Linux machine on the [benchmarks page](@/benchmarks.md).

Use `--summary` in a script to decide whether a diff is worth writing, or to see how many segments will need refining. The exit status follows [diff](@/guide/exit-codes.md): 1 when the files differ.
