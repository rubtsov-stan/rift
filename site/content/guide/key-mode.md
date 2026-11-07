+++
title = "Key mode: compare CSV, NDJSON and pg_dump by key"
description = "rift --key matches records by key and ignores row order. Works on CSV columns, NDJSON fields and COPY blocks of a pg_dump file."
weight = 4
[extra]
nav = "Key mode"
h1 = "Key mode"
lead = "With --key, Rift matches records by key and ignores row order. Use it on exports where the same rows can arrive in a different order."
summary = "Compare records by key: CSV columns, NDJSON fields, pg_dump COPY blocks."
+++

A line diff reports a reordered export as thousands of deletions and insertions. Key mode reports what changed per record.

## Choose the key

| Spec | Key |
|---|---|
| `csv:N` | CSV column `N`, counted from 1. |
| `csv:NAME` | CSV column whose header is `NAME`. |
| `json:PATH` | A dotted field path in NDJSON records, such as `user.id`. |
| `copy:NAME` | Column `NAME` of the `COPY` blocks of a `pg_dump` file. |

## Read the output

Rift prints one line per key that differs, with tabs between the fields and line numbers counted from 1:

| Line | Meaning |
|---|---|
| `~KEY<tab>A<tab>B` | The key is in both files and the records differ. |
| `-KEY<tab>A<tab>-` | The key is only in the first file. |
| `+KEY<tab>-<tab>B` | The key is only in the second file. |

`A` and `B` are the lines where the record starts, or `-` when it is absent.

{{ media(ready=true, mp4="key-mode.mp4", gif="key-mode.gif", poster="key-mode-poster.jpg", width=960, height=400, alt="rift --key csv:id on a pair of CSV files") }}

This is a pair of generated CSV files, 32 MiB each, with the same keys in a different order and some rows added and removed:

{{ term(name="csv-head", title="a.csv has a header row") }}

{{ term(name="key-csv", kind="keys", title="rift --key csv:id") }}

The last line of that output is the line count of `keys.txt`, the number of keys that differ.

The same on NDJSON, by the `id` field. The key prints as the file has it, quotes included:

{{ term(name="ndjson-sample", title="a.ndjson") }}

{{ term(name="key-ndjson", kind="keys", title="rift --key json:id") }}

## Rules to know

- Rift compares the bytes of a record before it calls the record unchanged.
- In CSV, a record can span lines inside quotes, and the header row counts as a record keyed by its own cell.
- In a `pg_dump`, only the rows of `COPY` blocks with the right column count take part, and the same key in two tables names two records.
- A key that occurs twice in one file stops the run with exit status 2 and lists the first few. A run can stop after it printed some changes. The output is then incomplete: check the exit status before you use it.
- A record too long for the budget stops the run with a budget error that names a `--mem` under which the record fits.
- `--key` cannot be combined with `--summary`, `--interactive` or `--no-moves`.

## Memory

Key mode writes no temporary files. When the keys do not fit `--mem`, Rift splits them by hash and reads both files once per part, so a smaller budget means more passes over the files. Each pass prints its changes sorted by line. `--stats` reports the number of passes.
