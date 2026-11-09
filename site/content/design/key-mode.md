+++
title = "Key mode: passes over key hash ranges, no temp files"
description = "How rift --key compares records by key in any row order: passes over ranges of key hashes, 5 bytes per key, no temporary files, and duplicate keys."
weight = 5
[extra]
nav = "Key mode"
h1 = "Key mode"
lead = "A dump with its rows in another order is a different file to a line diff. rift --key compares records by key instead, reading both files once per pass and writing nothing to disk."
summary = "Multi-pass hash ranges with no temporary files, why more --mem means fewer passes, and duplicate keys."
+++

[DESIGN.md](https://github.com/rubtsov-stan/rift/blob/main/DESIGN.md) describes this mode under "Key mode". The [key mode guide](@/guide/key-mode.md) shows how to run it.

## Records and keys

`--key` names where the key is: a CSV column by number or header name (`csv:1`, `csv:id`), a field path in NDJSON (`json:user.id`), or a column of the `COPY ... FROM stdin` blocks of a `pg_dump` (`copy:id`). A record is one line, except in CSV, where a quoted field can hold line breaks. The record's bytes leave out the line terminator, so a change from `\n` to `\r\n` alone is not a change. Each key gets a 63-bit hash taken from xxh3 of its bytes.

Sources: `crates/rift-core/src/keyed/mod.rs`, `KeySpec::from_str`; `crates/rift-core/src/keyed/read.rs`, module docs and `key_hash`.

## One pass, one range of key hashes

<figure class="diagram">
<svg viewBox="0 0 720 250" role="img" aria-labelledby="key-t key-d">
<title id="key-t">Key mode passes over ranges of key hashes</title>
<desc id="key-d">A bar for the range of key hashes, split into four passes. The first pass ran out of room and started over on a narrower range. Below, files A and B, each read from start to end in every pass. A record whose key hash falls in the pass's range waits in memory until its key appears in the other file; the two records are then compared byte by byte, and only a 4-byte fingerprint of the key stays in the table.</desc>
<text x="10" y="20" font-weight="700">key hashes, 0 to 2^63</text>
<rect class="box" x="10" y="30" width="700" height="30"/>
<rect x="10" y="30" width="150" height="30" style="fill:var(--accent)"/>
<path class="link" d="M160 30 V60 M330 30 V60 M520 30 V60" />
<path stroke-dasharray="4 3" d="M300 26 V64" style="stroke:var(--red);stroke-width:2;fill:none"/>
<text x="16" y="50" style="font-size:12px;fill:var(--bg)">pass 1</text><text x="200" y="50" style="font-size:12px">pass 2</text><text x="400" y="50" style="font-size:12px">pass 3</text><text x="580" y="50" style="font-size:12px">pass 4</text>
<text x="182" y="80" style="font-size:12px;fill:var(--red)">pass 1 first tried up to here, ran out of room, started over narrower</text>
<text x="10" y="114" font-weight="700">A</text><rect class="box" x="40" y="100" width="670" height="20"/>
<text x="10" y="154" font-weight="700">B</text><rect class="box" x="40" y="140" width="670" height="20"/>
<path d="M80 100 V120 M190 100 V120 M260 100 V120 M410 100 V120 M600 100 V120 M120 140 V160 M300 140 V160 M450 140 V160 M560 140 V160 M650 140 V160" style="stroke:var(--accent);stroke-width:3"/>
<path class="link" d="M40 176 H700" marker-end="url(#key-arrow)"/>
<defs><marker id="key-arrow" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="8" markerHeight="8" orient="auto"><path d="M0 0 L8 4 L0 8 Z" style="fill:var(--muted)"/></marker></defs>
<text x="40" y="196" style="font-size:12px">every pass reads A and B side by side, start to end, keeping only records in its range</text>
<text x="40" y="218" style="font-size:12px">a record waits, bytes and all, until its key shows up in the other file;</text>
<text x="40" y="238" style="font-size:12px">then the two are compared and only a 31-bit fingerprint stays in a 4-byte slot</text>
</svg>
<figcaption>Highlighted ticks are records whose key hash falls in pass 1. Records outside the range are parsed and skipped; a later pass picks them up.</figcaption>
</figure>

`keyed::compare` works in passes. Each pass owns a range of key hashes and reads A and B side by side, a window of each at the same relative position, so files in roughly the same order meet their keys close together. It skips every record whose key hash lies outside its range.

A record in the range goes into a hash table. If its key is not there, the record waits, bytes included, in an arena. When the same key turns up in the other file, Rift compares the two records byte by byte, marks the key as changed when they differ, and frees the waiting bytes for reuse. The table slot keeps only a 31-bit fingerprint of the key hash, in four bytes, to catch a third occurrence. The table is sized at 5/4 of the keys it expects and filled to 80%, which is 5 bytes per key. At the end of a pass, records still waiting are the removed keys (from A) and the added ones (from B), and the pass prints its changes in line order.

Memory follows the two halves. The tables come from the skeleton half. The read windows, the lists of records they hold and the waiting records come from the refine half. A record is compared and counted as unchanged only after its bytes match.

Sources: `crates/rift-core/src/keyed/pass.rs`, `compare`, `read_pass`, `Pass::new`, `Pass::take`, `Pass::matched`, `fingerprint`, `emit`; `Sizes::new`.

## Running out of room

A fingerprint cannot say which keys lie above a new top of the range, so a full table cannot be thinned in place, the way the scan thins a sample. When a table or the waiting records outgrow the budget, the pass starts over on a narrower range, sized from how far it had read:

- A full table means the range held too many keys. Tables fill as the files are read, so the new range is the share the table should hold to the end, with a 10% margin.
- Full waiting records mean the files are far out of order. The range is at least halved, and later passes may not widen past 90% of it.

A pass that ends with its tables under 90% full widens the next one toward 90%, at most twofold. Each pass rereads both files. Nothing goes to a temporary file, so key mode needs no disk space and no cleanup, and its cost is time: one read of both files per pass.

Sources: `pass.rs`, `compare` (the `Outcome::Overflow` and `Outcome::Done` arms), `narrowed`, `fill`, `scale`.

## More memory, fewer passes

The table's capacity is what the skeleton half holds at 4 bytes a slot, and the passes needed are about the key count divided by what the table holds. Doubling `--mem` doubles the keys per pass until one pass covers all of them; beyond that, more memory changes nothing.

On a 256 MiB pair from `rift-gen --seed 7 --size 256M --profile shuffled --rows csv` (3,350,294 records in A, 3,349,981 in B), `rift --key csv:id --stats`:

| `--mem` | Passes | Peak heap | Time |
|---|---|---|---|
| 8M | 5 | 8.0 MiB | 1.88 s |
| 16M | 3 | 15.9 MiB | 0.68 s |
| 32M | 2 | 31.9 MiB | 0.57 s |
| 64M | 1 | 63.5 MiB | 0.57 s |
| 256M | 1 | 74.3 MiB | 0.41 s |
| 1G | 1 | 74.3 MiB | 0.41 s |

Every run reported the same 100,154 changed keys; the order of the lines differs between pass counts, since each pass prints its own changes, and sorted outputs are identical. Times are single runs with a warm page cache; three more runs at 8M took 2.00 to 2.68 s, and at 64M 0.39 to 0.53 s. In 0.3.0, below 16 MiB of `--mem` one thread did all the work, which accounts for part of the jump from 16M to 8M. Measured 2026-11-07 on an Apple M1, 16 GiB, macOS 27.0.1.

DESIGN.md records the large case: the 10 GiB `shuffled` CSV pair at `--mem 256M` takes 6 passes and about 41 s on the M1 with files larger than the page cache, each pass at about the speed of the disk.

Sources: `pass.rs`, `Pass::new`, `Pass::shards`, `plan`.

## Threads

The work is spread twice. A window is cut into chunks at line breaks, and every chunk is split into records on its own thread, assuming a record starts at its first byte and the parser is in the state the window started in. A chunk counts only if the one before it ended where it starts, in that state. A quoted line break at a cut, or a `COPY` header inside the window, makes the rest parse again; plain rows never do. Then the pass's range is split between shards by bits of the key hash the table does not use, and each shard has a table and waiting records of its own.

The shard count comes from `--mem` alone: one per MiB of the refine half, from 1 to 8. The shards run on a thread pool of their own, one thread per shard, at most one per core or `RAYON_NUM_THREADS`; its heap, 16 KiB per shard, comes from the refine half. The threads decide only who runs which shard, so a machine with fewer cores runs the same passes, more slowly.

Lists that grow at the same time never race for the refine half. Before each round, what the half has free is split into equal credits, one per chunk list of the two windows and one per shard, and a list grows only within its own. Whether a shard runs out, and so where a pass starts over, depends on its records alone. The same files under the same `--mem` print the same bytes on every run, on any machine.

Sources: `read.rs`, module docs, `grow`; `pass.rs`, `shard`, `plan`, `workers`, `read_pass`.

## Duplicate keys

Key mode needs each key once per file. A key that turns up a second time from the same file while its first record waits is a duplicate. A key whose fingerprint matches a key already matched is only a suspect: the matched key's bytes are gone, so the fingerprint might belong to another key. The next pass counts the suspects in their file, or, after the last pass, one more read does. A pass that piles up too many suspects stops and counts them at once.

Duplicates end the run with exit status 2 and the first five keys:

```
$ rift --key csv:id a.csv b.csv
rift: duplicate keys: 1
```

Since a suspect is settled one pass later, a duplicate can surface after the changes of its pass were printed. The command line then adds that "the changes printed above are incomplete and must be discarded".

Sources: `pass.rs`, `Pass::duplicate`, `Pending`, `collect_dups`, `FEW`; `keyed/mod.rs`, `KeyError`; `crates/rift-cli/src/keyed.rs`, `incomplete`.

## Why not keep A's records whole

The plain way to compare by key is to partition both files by key hash and hold one side's records in a table. DESIGN.md measured that layout at about 28 times the memory per record on `shuffled` CSV, 138 bytes against 5 for an 81-byte row, and so about as many more passes. Holding only fingerprints of matched keys, and the bytes of records still waiting, is what keeps a pass at a few bytes per key.
