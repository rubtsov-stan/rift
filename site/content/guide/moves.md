+++
title = "Moved blocks and --no-moves: how Rift reports them"
description = "Rift lists blocks of three or more lines that moved between two files. The unified diff is unchanged; --summary shows them, --no-moves skips the search."
weight = 5
[extra]
nav = "Moves"
h1 = "Moved blocks and --no-moves"
lead = "A block that moved shows as a deletion and an insertion in the diff. Rift also tells you it moved."
summary = "What the moved block list means, what it misses, and how to turn it off."
+++

A dump written in a different order, or a function cut from one place and pasted into another, is a deletion at one end and an insertion at the other in any line-based diff. Rift keeps that output, because the diff must still apply with `patch`. It adds a list of the blocks that moved.

## Where the list appears

- `--summary` and `--stats` print it.
- The terminal view marks both ends of a block, and `m` jumps from one end to the other.
- The unified diff does not change, byte for byte.

A block counts as moved when it has at least three lines and appears in both files at different positions. No line is in two blocks. In the summary of the generated pair:

{{ term(name="sql-summary", kind="summary", title="the moved section is near the end") }}

Each range is `a start..end  b start..end`, counted from 0 with the end excluded.

## What it misses

Detection starts from sampled lines, so it finds only the blocks that contain a sampled line. Some moves are missed. Do not use the list to prove that nothing else moved.

When the budget leaves no room for the search, the output says so. When the list of blocks fills up, the search keeps the blocks found so far and the output says later moves are not listed.

## Turn it off

`--no-moves` skips the search. The unified diff is the same. The summary, the stats and the terminal view say that detection was off:

{{ term(name="sql-no-moves", kind="summary", title="rift --no-moves") }}

`--no-moves` cannot be combined with `--key`: key mode ignores order and has no moves to find.
