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

## Print the records

With `--show-records`, Rift prints the records under each change line. The record from the first file follows `  - `, the one from the second file follows `  + `:

```
~1	2	4
  - 1,x
  + 1,X
-2	3	-
  - 2,"two
  - lines"
+4	-	3
  + 4,"say ""hi"""
```

A record that spans lines carries the prefix on each line. Strip the prefixes and you get the record's bytes as the file has them, quotes and inner line breaks included, without the line terminator. Rift reads each record back from its file through a 64 KiB buffer, so a long record takes no more memory than a short one.

`--show-records` prints whole records. Output that names the fields that changed is planned for 0.4, see the [roadmap](@/roadmap.md).

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
- In CSV, a record can span lines inside quotes, and the header row counts as a record keyed by its own cell. A UTF-8 byte order mark before the header is not part of it.
- Blank lines are not records, and in NDJSON neither are lines of only spaces or tabs.
- In a `pg_dump`, only the rows of `COPY` blocks with the right column count take part, and the same key in two tables names two records.
- A key that occurs twice in one file stops the run with exit status 2 and lists the first few. A run can stop after it printed some changes. The output is then incomplete: check the exit status before you use it.
- A budget too small stops the run with a budget error that names a `--mem` to rerun with: one under which a long record fits, and otherwise about twice the limit. Following the advice reaches a limit that works in a few reruns.
- `--key` cannot be combined with `--summary`, `--interactive` or `--no-moves`. `--show-records` needs `--key`, and `--stats` works with both.

## Memory

Key mode writes no temporary files. When the keys do not fit `--mem`, Rift splits them by hash and reads both files once per part, so a smaller budget means more passes over the files. Each pass prints its changes sorted by line. `--stats` reports the number of passes.

Key mode prints the same set of lines at any `--mem`. Their order depends on `--mem`, since the passes split the keys by budget. From 0.3.3 it depends on nothing else: the same files under the same limit print the same bytes on every run, on any machine and any number of threads. Up to 0.3.2 it can also change between runs once key mode uses several threads (see Known issues). Sort the output before you compare runs with different limits.

Key mode runs at most 64 passes. Each time a pass ends or starts over on a narrower range, Rift works out the fewest passes the rest of the run can take at this `--mem`: the records each file has shown so far whose keys belong to later passes, divided by what one pass's tables hold. If that is more than 64, it stops with the usual budget error. That can happen after some passes have printed their changes: the output is then incomplete, the message says so, and the exit status is 2. On a 1 MiB CSV pair from `rift-gen --seed 7 --profile shuffled --rows csv --change-ratio 0.01`, which takes 152 passes at `--mem 64K` without the limit:

```
$ rift --key csv:id --mem 64K a.csv b.csv
rift: refine budget exceeded: requested 21504 bytes, 8192 available; rerun with --mem 90K
```

Each refusal names a `--mem` about twice as large or more; at 90K that pair takes 42 passes. Following the advice reaches a limit that works in a few reruns. The count is of records already read, so a run that fits in 64 passes is never refused, whatever its keys, and the same files under the same `--mem` get the same answer on any machine. Without the limit, a budget far too small for the files would still finish, after hundreds of passes over both files.

Why 64: each pass rereads both files, and a pass takes about the same time at any budget, about 0.14 s on a 128 MiB CSV pair, 0.08 s on a 64 MiB SQL dump and 0.05 s on a 64 MiB NDJSON file (Apple M1, warm page cache). The limit keeps the reading to at most 64 times one pass, whatever the file size. On those pairs it refuses only the CSV and SQL runs at `--mem 256K`, of 8 to 15 s, and the advised limit cuts them to about a third. On large files 64 passes can still take minutes, which is what such a budget asks for.

`--show-records` takes 64 KiB of the budget for its read buffer, plus 24 bytes for each record that waits for its match or has changed, until the end of its pass. Under a tight `--mem` that can add passes.

## Known issues

- Up to 0.3.2, with several threads, from about `--mem 16M`, the order of the lines can differ between two runs of the same command. The set of lines is always the same. 0.3.3 fixes this. Until then, sort the output before you compare runs.
- Rift 0.3.1 and 0.3.2 run key mode on one thread at exactly `--mem 16M`: the process reserve takes the refine half just under the 8 MiB that several threads need. On a 128 MiB CSV pair that took 2.21 s where 0.3.0 took 1.25 s. `--mem 16528K` or more restores the old time. The measurement is from the 0.3.1 changelog.
