# The `.riftdiff` format, version 1.0

Experimental: the format may change before Rift 1.0. A result is written
by `rift A B --save FILE` (also with `--summary`) and read by `rift open`
and `rift info`.

A result holds the structure of a diff, not its text: the segment map,
the edit script of every refined segment, the moved blocks, the sparse
line index of each file, and an xxh3-64 hash of every 1 MiB chunk of each
file. Text is read from the two files, and every read is checked against
the chunk hashes. xxh3 catches files that changed; it does not stand up
to a file made to collide.

## Conventions

Integers are little-endian. `uvar` is unsigned LEB128, at most 10 bytes;
`svar` is a zigzag-coded `uvar`. Offsets are absolute positions in the
result file. Every checksum is xxh3-64 with seed 0.

## Layout

```
header           64 bytes
chunk table A    8 bytes per chunk of A
chunk table B    8 bytes per chunk of B
segment blocks   the SEGMENTS section
other sections   in any order
section table    32 bytes per entry
footer           32 bytes
```

A file whose last 8 bytes are not `RIFTEND\0` is incomplete and is
refused. Rift writes `FILE.tmp`, syncs it and renames it over `FILE`.

### Header

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | magic `RIFTDIFF` |
| 8 | 2 | format major, 1 |
| 10 | 2 | format minor, 0 |
| 12 | 4 | flags: bit 0 the result holds edit scripts, bit 1 every differing segment has one, bit 2 a MOVES section is present |
| 16 | 4 | chunk size as a power of two, 20 |
| 20 | 4 | reserved, zero |
| 24 | 8 | offset of chunk table A |
| 32 | 8 | chunks in A |
| 40 | 8 | offset of chunk table B |
| 48 | 8 | chunks in B |
| 56 | 8 | checksum of bytes 0 to 55 |

### Footer

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | offset of the section table, which ends where the footer starts |
| 8 | 4 | entries in the section table, at most 64 |
| 12 | 4 | reserved, zero |
| 16 | 8 | checksum of the section table |
| 24 | 8 | magic `RIFTEND\0` |

### Section table

Each entry is 32 bytes: `type u16`, `flags u16` (bit 0 critical),
`reserved u32`, `offset u64`, `length u64`, `checksum u64` of the
section's bytes. A section lies between the header and the section
table. A type appears at most once.

| Type | Name | Critical | Contents |
|---|---|---|---|
| 1 | META | yes | identity, parameters and summary |
| 2 | BLOCK_INDEX | yes | one entry per segment block |
| 3 | SEGMENTS | yes | the segment blocks, back to back |
| 4 | LINES | yes | the line indexes of A and B |
| 5 | MOVES | no | moved blocks |
| 6 | CHUNKS_A | yes | chunk table A, as in the header |
| 7 | CHUNKS_B | yes | chunk table B |
| 8 | CHUNKS_CDC_A | no | reserved: content-defined chunks of A with xxh3-128 hashes, for an incremental re-diff |
| 9 | CHUNKS_CDC_B | no | reserved: the same for B |

Version rules: a reader refuses a major it does not know. It skips a
section type it does not know unless the entry is critical, and then it
refuses the file. A minor version adds only non-critical sections and
meta records; anything a reader must understand to show a correct diff
takes a new major.

### Chunk tables

Chunk `k` of a file of `n` bytes covers bytes `k << 20` up to
`min((k + 1) << 20, n)`; there are `ceil(n / 2^20)` chunks. Entry `k` is
the xxh3-64 of those bytes, as `u64`. The scan writes each entry in
place as soon as its chunk is hashed. The chunk size does not depend on
`--mem` or on the thread count, so two saves of the same file hold the
same table.

### META

A sequence of records `tag uvar, length uvar, bytes`. A reader skips a
tag it does not know.

| Tag | Contents |
|---|---|
| 1, 2 | file A, file B: `size uvar`, `mtime seconds svar`, `mtime nanoseconds uvar`, `dev uvar`, `ino uvar`, `path length uvar`, path as given (raw bytes), `canonical length uvar`, canonical path (raw bytes, empty if unknown) |
| 16 | `--mem` in bytes, `uvar` |
| 17 | 1 if moved blocks were looked for, `uvar` |
| 18 | the Rift version that wrote the file, UTF-8 |
| 32 | lines of A and of B, two `uvar` |
| 33 | segments that are equal, differ, approximate: three `uvar` |
| 34 | refined segments whose script is approximate, by reason (cutoff, no anchor, budget): three `uvar` |
| 35 | differing segments that have a script, `uvar` |

Tags 1 and 2 differ between copies and machines. The other records, and
every other section except the chunk tables' placement, depend only on
the inputs, `--mem` and the Rift version. No time of writing and no host
name are stored.

### Segment blocks

The SEGMENTS section is a run of blocks. A block closes at the first
segment boundary after 64 KiB; one segment's record is never split, so
a segment with a long script makes a larger block.

```
kind          u8     1
flags         u8     0; anything else is an encoding this version does not read
first segment uvar
line A        uvar   first line of A in the block
line B        uvar
byte A        uvar   first byte of A in the block
byte B        uvar
records       one per segment
checksum      u64    of every byte of the block before it
```

The number of records is the next block's first segment, or the total
from META for the last block, minus this block's first segment.

A segment record:

```
tag       u8     bits 0-1 status: 0 equal, 1 differs, 2 approximate
                 bit 2 an edit script follows
                 bits 3-4 0, or 1 + the script's approximation reason
                 bits 5-7 zero
len A     uvar   lines
len B     uvar   lines; absent for an equal segment, which has len A
bytes A   uvar   bytes of the A range
bytes B   uvar   absent for an equal segment
edits     uvar   number of edits, only when bit 2 is set
edit      uvar   (lines << 2) | kind, kind 0 equal, 1 delete, 2 insert
```

Segments tile both files: a segment starts where the previous one ends,
and at least one of its ranges is not empty. Edits are contiguous, so
they need no positions: walking them from the segment's start covers its
A range through equal and delete runs and its B range through equal and
insert runs, exactly. An equal segment never has a script; it is one
equal run.

### BLOCK_INDEX

One 32-byte entry per block, in order: `first segment u64`,
`first line of A u64`, `first line of B u64`, `offset u64`. A block ends
where the next begins; the last ends with the SEGMENTS section. The
first block starts the section.

### LINES

For A, then B: `interval uvar`, `lines uvar`, `count uvar`, then `count`
checkpoints as `line delta uvar, offset delta uvar` from the previous
one (the first from 0). A checkpoint is the start of a line. The first
is line 0 at offset 0 whenever the file has lines; lines and offsets
increase, and no two checkpoints fall in the same `interval`-byte block.
This is the scan's sparse index: a reader finds any line with one
binary search and a forward read of less than `interval` bytes.

### MOVES

`flags u8` (bit 0 the search was off, bit 1 skipped for the budget,
bit 2 its list filled up), `count uvar`, then per block, in A order:
`start in A uvar` as the distance from the previous block's end in A,
`start in B uvar`, `length uvar`.

## Opening a result

`rift open` compares each file's size, modification time, device and
inode with META. A size that differs refuses the result as stale. Any
other difference opens it as unverified. In both other cases every read
of text loads the whole 1 MiB chunk it falls in, hashes it, compares the
hash with the table and serves the text from the same buffer; after one
mismatch nothing more is read from that file.
