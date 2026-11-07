+++
title = "Compare huge CSV files by key, ignoring row order"
description = "rift --key csv:id compares two CSV exports by a column and ignores row order. Output shows added, removed and changed keys with line numbers."
weight = 2
[extra]
nav = "Compare CSV by key"
h1 = "Compare huge CSV files by key"
lead = "Two exports of the same table rarely list their rows in the same order. A line diff then reports everything as changed."
summary = "Match rows by a column and list the keys that were added, removed or changed."
+++

Pick the key column by header name or by position:

```
rift --key csv:id a.csv b.csv
rift --key csv:1 a.csv b.csv
```

On a pair of generated CSV files, 32 MiB each, with a header row:

{{ term(name="csv-head", title="the header row and the first record") }}

{{ term(name="key-csv", kind="keys", title="rift --key csv:id") }}

Each line is one key that differs, with tabs between the fields. `+` is a key only in the second file, `-` a key only in the first, `~` a key in both with different records. The two numbers are the line where the record starts in each file, from 1. The last line is the line count: the number of keys that differ.

## Check the exit status

A key that occurs twice in one file stops the run with exit status 2. The run can stop after it printed some changes, so a script must check the exit status before it uses the output. See [exit codes](@/guide/exit-codes.md).

## Memory

Key mode writes no temporary files. When the keys do not fit the budget, Rift splits them by hash and reads both files once per part. A 1 GiB CSV pair takes two passes at the default 256 MiB. `--stats` reports the number of passes.

More options and rules are in the [key mode guide](@/guide/key-mode.md).
