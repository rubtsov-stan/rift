+++
title = "Compare NDJSON logs: line diff and diff by field"
description = "Diff two large NDJSON files with rift: a line diff for append-only logs, or --key json:id to match records by a field and ignore their order."
weight = 5
[extra]
nav = "Compare NDJSON logs"
h1 = "Compare NDJSON logs"
lead = "NDJSON has one JSON object per line, so a line diff works. When the order changes, match by a field instead."
summary = "Line diff for NDJSON, and key mode to match records by a field."
+++

## A line diff

On a pair of generated event logs, 64 MiB each:

{{ term(name="ndjson-summary", kind="summary", title="a.ndjson, b.ndjson") }}

`rift a.ndjson b.ndjson` writes the unified diff, and `patch` applies it. The summary above peaked at a heap of 68.0 MiB.

## Match by a field

When the same events can arrive in a different order, compare by key. `json:PATH` takes a dotted path, so `json:user.id` reaches a nested field. Here the records have an `id` field:

{{ term(name="ndjson-sample", title="the first two records") }}

{{ term(name="key-ndjson", kind="keys", title="rift --key json:id") }}

The key prints as the file has it, quotes included. A tab, line break or backslash in a key prints as `\t`, `\n` or `\\`. See the [key mode guide](@/guide/key-mode.md) for the output format and the rules.
