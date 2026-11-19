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

With `copy:NAME`, the key of every line is prefixed by its table, as the `COPY` header names it, and a colon, so the same key in two tables prints as two keys:

```
~public.item:1	2	2
~public.order:1	5	5
-public.order:2	6	-
```

Tabs, line breaks and backslashes in the table name and the key are escaped as `\t`, `\n`, `\r` and `\\`, and a colon in the table name as `\:`. Read the escapes from the left: the first colon that is not part of an escape ends the table name.

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

## Show the fields that changed

With `--show-fields`, Rift prints under each changed key the fields that differ, one per line, four spaces in:

```
$ rift --key csv:id --show-fields a.csv b.csv
~1	2	4
    price: 10 -> 12
    note: "red, sweet" -> "red, sour"
~3	3	2
    note: "two\nlines" -> "two\nLINES"
-4	5	-
+5	-	5
```

A field is a CSV column, a top-level member of an NDJSON object, or a column of a `COPY` row. Added and removed keys get no field lines.

- Fields are named by each file's own header row (`csv:NAME`) or `COPY` column list, and matched by name, so a column that moved is not a change. With `csv:N` the file may have no header, so fields are numbered from 1: `3: 10 -> 12`.
- Values print as the file has them: CSV quotes stay, NDJSON values are raw JSON, and `\N` stays `\N`. A tab, line break or backslash prints as `\t`, `\n` or `\\`. A value longer than 64 bytes prints as a window of at most 64 bytes around the first byte where it differs from the other side, so the change is on screen. The window starts at the value's start when the difference is in its first 48 bytes, and otherwise 48 bytes before it; both sides use the same start. A cut end is marked `...`. The window never begins or ends inside a UTF-8 character. The window follows the bytes as printed, so for a CSV cell quoted on one side only, a change after byte 64 can still be cut ([#108](https://github.com/rubtsov-stan/rift/issues/108)). `--show-records` prints the value whole.
- A CSV cell is compared without its quotes, so `"10"` and `10` are the same value. An unquoted empty cell, which a Postgres CSV export writes for NULL, differs from `""`. NDJSON values and `COPY` cells compare as their bytes, so `{"n":1}` and `{"n": 1}` differ, and so do `\N` and an empty cell.
- An NDJSON member only one record has shows `(none)` on the other side, as does a CSV cell past the end of a short row. A nested object or array is one field.
- When the records differ but no field does, as with NDJSON members in another order, the line under the key reads `(no field differs)`. An NDJSON line that is not one JSON object reads `(record not split into fields)`.
- With `--show-records` too, the field lines come first, then the records.

## Compare some fields only

`--fields price,qty` compares only the fields named, and implies `--show-fields`. A key whose other fields changed is not printed, and if no key is left the exit status is 0. Added and removed keys are still printed. Names are header names, `COPY` column names or top-level NDJSON names; with `csv:N` they are column numbers. A name that no header holds stops the run with exit status 2. NDJSON has no header, so a misspelt member name matches nothing.

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
- A key that occurs twice in one file (in a `pg_dump`, twice in one table) stops the run with exit status 2 and lists the first few, each behind its table in a `pg_dump`: `duplicate keys: public.item:7`. A run can stop after it printed some changes. The output is then incomplete: check the exit status before you use it.
- A budget too small stops the run with a budget error that names a `--mem` to rerun with: one under which a long record fits, and otherwise about twice the limit. Following the advice reaches a limit that works in a few reruns.
- `--key` cannot be combined with `--summary`, `--interactive` or `--no-moves`. `--show-records`, `--show-fields` and `--fields` need `--key`, and `--stats` works with both.

## From an MCP client

`rift-mcp` has the same comparison as two tools, `keyed_summary` and `keyed_changes`. A `COPY` table comes as a field of its own, not in front of the key. See the [MCP guide](@/guide/mcp.md).

## Memory

Key mode writes no temporary files. When the keys do not fit `--mem`, Rift splits them by hash and reads both files once per part, so a smaller budget means more passes over the files. Each pass prints its changes sorted by line. `--stats` reports the number of passes.

Key mode prints the same set of lines at any `--mem`. Their order depends on `--mem`, since the passes split the keys by budget. From 0.3.3 it depends on nothing else: the same files under the same limit print the same bytes on every run, on any machine and any number of threads. Up to 0.3.2 it can also change between runs once key mode uses several threads (see Known issues). Sort the output before you compare runs with different limits.

Key mode runs at most 64 passes. Each time a pass ends or starts over on a narrower range, Rift works out the fewest passes the rest of the run can take at this `--mem`: the records each file has shown so far whose keys belong to later passes, divided by what one pass's tables hold. If that is more than 64, it stops with the usual budget error. That can happen after some passes have printed their changes: the output is then incomplete, the message says so, and the exit status is 2. On a 1 MiB CSV pair from `rift-gen --seed 7 --profile shuffled --rows csv --change-ratio 0.01`, which takes 146 passes at `--mem 64K` without the limit:

```
$ rift --key csv:id --mem 64K a.csv b.csv
rift: refine budget exceeded: requested 22272 bytes, 8192 available; rerun with --mem 128K
```

A budget refusal can come before data errors such as duplicate keys are found, since those keys may lie in a part of the hash range no pass has reached. Each refusal names a `--mem` about twice as large or more; at 128K that pair takes 22 passes. Following the advice reaches a limit that works in a few reruns. The count is of records already read, so a run that fits in 64 passes is never refused, whatever its keys, and the same files under the same `--mem` get the same answer on any machine. Without the limit, a budget far too small for the files would still finish, after hundreds of passes over both files.

Why 64: each pass rereads both files, and a pass takes about the same time at any budget, about 0.14 s on a 128 MiB CSV pair, 0.08 s on a 64 MiB SQL dump and 0.05 s on a 64 MiB NDJSON file (Apple M1, warm page cache). The limit keeps the reading to at most 64 times one pass, whatever the file size. On those pairs it refuses only the CSV and SQL runs at `--mem 256K`, of 8 to 15 s, and the advised limit cuts them to about a third. On large files 64 passes can still take minutes, which is what such a budget asks for.

`--show-records` takes 64 KiB of the budget for its read buffer, plus 24 bytes for each record that waits for its match or has changed, until the end of its pass. Under a tight `--mem` that can add passes.

`--show-fields` takes the same 24 bytes per record. It reads each change's two records back into one buffer, with lists for their fields at 20 bytes a field. About 7 KiB of that, room for 4 KiB of records and 64 fields, is taken before the first pass; past that the buffer grows to the largest pair and stays to the end of the run. A pair that does not fit stops the run with a budget error that names a `--mem`; a change is never skipped. With `--fields`, a key whose named fields are equal still holds its 24 bytes until its pass ends.

## Known issues

- Up to 0.3.2, with several threads, from about `--mem 16M`, the order of the lines can differ between two runs of the same command. The set of lines is always the same. 0.3.3 fixes this. Until then, sort the output before you compare runs.
- Rift 0.3.1 and 0.3.2 run key mode on one thread at exactly `--mem 16M`: the process reserve takes the refine half just under the 8 MiB that several threads need. On a 128 MiB CSV pair that took 2.21 s where 0.3.0 took 1.25 s. `--mem 16528K` or more restores the old time. The measurement is from the 0.3.1 changelog.
