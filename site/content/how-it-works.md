+++
title = "How Rift works: sampled anchors and a memory budget"
description = "Rift hashes each line once, samples lines by hash, anchors on lines unique to both files, and refines differing segments on demand within a heap budget."
weight = 1
[extra]
nav = "How it works"
h1 = "How Rift works"
lead = "Rift splits the diff into a cheap map of the whole pair and expensive work on the places that differ. Neither is allowed to pass its share of the memory budget."
+++

The full design, with the invariants and the measurements behind them, is in [DESIGN.md](https://github.com/rubtsov-stan/rift/blob/main/DESIGN.md). This page is the short version.

{{ diagram() }}

## 1. Hash every line once

Rift reads each file in fixed buffers and gives every line a 64-bit xxh3 hash. A line belongs to a sample when the low bits of its hash are zero. The sample depends only on the line's content, so both files pick the same lines, whatever was inserted or deleted elsewhere. This is the same idea as content-defined chunking.

When the sample does not fit the budget, Rift thins it by asking for more zero bits, without reading the files again.

## 2. Anchors

An anchor is a sampled line that occurs exactly once in each file, as in patience diff. Rift compares the two lines byte by byte before it trusts an anchor: a hash match alone proves nothing.

## 3. The longest chain

Anchors that appear in the same order in both files form a chain. The longest chain cuts the pair into segments. In the diagram, the dashed line is an anchor that does not fit the order, so it is left out.

## 4. Equal and differing segments

A segment whose bytes are identical on both sides is `equal`, and Rift confirms it by a byte comparison. No line is ever marked equal without one. The others are `differs`.

## 5. Refine on demand

Rift refines a differing segment only when something needs it: the terminal view asks for the visible hunk, or the unified diff writer reaches it. It runs patience diff, then Myers diff with linear memory. A segment too large for the refinement budget is sampled again, more densely, inside the segment only. If even that does not fit, Rift replaces the region whole and marks it approximate in every output.

## The memory rule

With `B` as the budget, at every moment:

```
skeleton + index                          <= B / 2
active refinement work + hunk cache       <= B / 2
```

Rift reserves each allocation on the data path from the budget before it makes it. Files are read with `pread` into fixed buffers, never `mmap`, so the resident set does not grow with file pages. A counting allocator tracks the heap, and the tests assert the peak against `--mem`.

## What this costs you

The diff is not minimal, as with patience diff in git. A block of rows that moved shows as a deletion and an insertion in the unified diff, and detection of moved blocks, which starts from the sample, can miss some. The [guide](@/guide/moves.md) covers both.
