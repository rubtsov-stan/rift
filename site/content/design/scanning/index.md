+++
title = "Scanning: pread, xxh3 and strata of sampled lines"
description = "How Rift reads two large files in parallel chunks through fixed buffers, hashes each line with xxh3 and picks the sampling stratum from --mem."
weight = 1
[extra]
nav = "Scanning"
h1 = "Scanning"
lead = "The first pass reads both files once, hashes every line and keeps a sample whose size the budget decides. Nothing about the sample depends on where a line sits in the file."
summary = "pread into fixed buffers, parallel chunks, xxh3, and how the stratum follows from --mem."
+++

[DESIGN.md](https://github.com/rubtsov-stan/rift/blob/main/DESIGN.md) covers this stage under "Strata" and in the sizing notes of its module contracts.

## Reading: pread into fixed buffers

The core never opens a file. It reads through a `Source` trait with one method that matters, `read_at(buf, offset)`, modelled on `pread`: a read at a position into a buffer the caller owns. The command line implements it for a file with `std::os::unix::fs::FileExt::read_at`, which is `pread`, and asks the kernel for sequential read-ahead (`posix_fadvise` on Linux, `F_RDAHEAD` on macOS). Tests implement it for a byte slice.

Sources: `crates/rift-core/src/source.rs`, trait `Source`; `crates/rift-cli/src/source.rs`, `FileSource::open` and `FileSource::read_at`.

Rift does not map the files into memory. A mapped page that the program has read counts in its resident set until the kernel reclaims it, so the resident set would grow with the file while the heap limit stayed put. To see the size of the effect, count the lines of a 256 MiB file both ways in Python on an Apple M1: through `mmap` the process peaked at 270 MiB resident (283,328,512 bytes), and through `os.preadv` with one 1 MiB buffer at 15.3 MiB (16,007,168 bytes). [Alternatives](@/design/alternatives.md) has the full argument.

Measured 2026-11-07, Apple M1, 16 GiB, macOS 27.0.1, warm page cache: `/usr/bin/time -l python3 -I mm.py a mmap|pread` on `a` from `rift-gen --seed 7 --size 256M --profile shuffled --rows csv`. The script walks the file with `mmap.find(b"\n")`, or reads it with `os.preadv` into a 1 MiB `bytearray` and counts newlines.

## Parallel chunks

`scan` cuts a file into chunks of equal size. Each worker thread keeps one read buffer of a chunk plus 64 KiB, takes the next chunk number from a shared counter, and hashes every line that starts in its chunk, however far that line runs past the chunk's end. Taking chunks in file order keeps the reads close to sequential.

A worker cannot know the absolute line number of its first line until the chunks before it are counted. It numbers lines from zero within the chunk, stores the count per chunk, and when all workers finish, a prefix sum over the counts turns every sample's local line number into an absolute one.

`ScanConfig::for_pair` sizes this. A quarter of the skeleton half pays for read buffers while the scan runs. The chunk is that quarter divided by the core count, kept between a floor (4 MiB when the budget allows) and 16 MiB, and the thread count is the core count, or fewer when the buffers would not fit. At the default 256 MiB budget, each half is 128 MiB, the buffer pool 32 MiB.

The scan needs several cores. In the spike recorded in DESIGN.md, a single thread hashed short lines at 1.16 GB/s from a cold cache, against 3.25 GB/s for `cat` on the same 10 GiB file; four threads reached 3.32 GB/s and eight 3.43 GB/s, the speed of the disk. The Linux benchmark shows the same shape for `--summary` on a 1 GiB SQL pair in tmpfs: 1.08 s on one CPU, 0.72 s on two, 0.53 s on four and 0.51 s on all twelve.

Sources: `crates/rift-core/src/scan/mod.rs`, `ScanConfig::for_pair`, `scan_metered`, `Worker::run` and `Worker::scan_chunk`; DESIGN.md, "Spike results", H2; `bench/results/linux-intel-core-i5-10600-2026-11-06/report.md`, runs 036 to 047.

## xxh3 per line

`memchr` finds each `\n`, and xxh3-64 hashes the whole line with its terminator, so `a\n`, `a\r\n` and an unterminated `a` are three different lines, as in GNU diff. A line longer than the read buffer is hashed in streaming mode across reads, so a 1 GiB line costs no more memory than a short one.

Every line is hashed, but only some are kept. A kept line becomes a `Sample` of 24 bytes: the hash, the line number and the byte offset where the line starts. The scan also keeps a checkpoint, a line number and an offset (16 bytes), every `C` lines, so any line can be reached later by a binary search and a forward scan of fewer than `C` lines.

Sources: `scan/mod.rs`, `Worker::line`; `crates/rift-core/src/sample.rs`; `crates/rift-core/src/index/mod.rs`, `Checkpoint` and `IndexedLines`.

## Strata

A line belongs to stratum `L` when the low `L` bits of its hash are zero. Stratum 0 holds every line, stratum 1 about half, stratum 2 about a quarter. Each stratum is a subset of the one below it, and membership depends on the bytes of the line alone.

That last property is what makes the sample useful. Insert three lines into B, and the lines after them move, but each keeps its hash, so the same lines are sampled in A and in B. This is the principle of content-defined chunking, applied to lines.

<style>#strata-fig .cell{fill:var(--accent);stroke:var(--rule)}#strata-fig .ins{stroke:var(--green);stroke-width:2}#strata-fig .del{stroke:var(--red);stroke-width:2}#strata-fig .ln{stroke:var(--accent);stroke-width:1.5;fill:none}#strata-fig[data-l="1"] .cell[data-t="0"]{fill:var(--panel)}#strata-fig[data-l="1"] .ln[data-t="0"]{stroke-opacity:.12}#strata-fig[data-l="2"] .cell[data-t="0"],#strata-fig[data-l="2"] .cell[data-t="1"]{fill:var(--panel)}#strata-fig[data-l="2"] .ln[data-t="0"],#strata-fig[data-l="2"] .ln[data-t="1"]{stroke-opacity:.12}#strata-fig[data-l="3"] .cell[data-t="0"],#strata-fig[data-l="3"] .cell[data-t="1"],#strata-fig[data-l="3"] .cell[data-t="2"]{fill:var(--panel)}#strata-fig[data-l="3"] .ln[data-t="0"],#strata-fig[data-l="3"] .ln[data-t="1"],#strata-fig[data-l="3"] .ln[data-t="2"]{stroke-opacity:.12}#strata-fig[data-l="4"] .cell[data-t="0"],#strata-fig[data-l="4"] .cell[data-t="1"],#strata-fig[data-l="4"] .cell[data-t="2"],#strata-fig[data-l="4"] .cell[data-t="3"]{fill:var(--panel)}#strata-fig[data-l="4"] .ln[data-t="0"],#strata-fig[data-l="4"] .ln[data-t="1"],#strata-fig[data-l="4"] .ln[data-t="2"],#strata-fig[data-l="4"] .ln[data-t="3"]{stroke-opacity:.12}@media (prefers-reduced-motion:no-preference){#strata-fig .cell,#strata-fig .ln{transition:fill .25s,stroke-opacity .25s}}</style>
<figure class="diagram" id="strata-fig" data-l="2">
<svg viewBox="0 0 720 230" role="img" aria-labelledby="strata-t strata-d">
<title id="strata-t">The same lines are sampled in both files</title>
<desc id="strata-d">Two rows of cells, one cell per line. File A has 32 lines. File B has the same lines with three lines inserted after line 8, line 15 deleted and line 24 changed. Filled cells are the lines in stratum 2: the 7 lines whose xxh3 hash ends in two zero bits. Lines join each sampled line of A to the same line in B. All 7 sampled lines of A are sampled in B too, at shifted positions, and the inserted and changed lines of B are not in the stratum.</desc>
<text x="4" y="34" font-weight="700">A, 32 lines</text>
<rect class="cell" data-s="a" data-t="4" x="4" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="25" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="46" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="67" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="1" x="88" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="1" x="109" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="130" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="151" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="172" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="193" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="3" x="214" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="235" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="1" x="256" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="1" x="277" y="44" width="19" height="24"/>
<rect class="cell del" data-s="a" data-t="0" x="298" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="319" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="2" x="340" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="361" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="382" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="4" x="403" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="424" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="445" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="466" y="44" width="19" height="24"/>
<rect class="cell del" data-s="a" data-t="0" x="487" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="508" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="529" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="2" x="550" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="1" x="571" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="0" x="592" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="2" x="613" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="4" x="634" y="44" width="19" height="24"/>
<rect class="cell" data-s="a" data-t="1" x="655" y="44" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="4" x="4" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="25" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="46" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="67" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="1" x="88" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="1" x="109" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="130" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="151" y="164" width="19" height="24"/>
<rect class="cell ins" data-s="b" data-t="0" x="172" y="164" width="19" height="24"/>
<rect class="cell ins" data-s="b" data-t="1" x="193" y="164" width="19" height="24"/>
<rect class="cell ins" data-s="b" data-t="0" x="214" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="235" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="256" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="3" x="277" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="298" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="1" x="319" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="1" x="340" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="361" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="2" x="382" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="403" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="424" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="4" x="445" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="466" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="487" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="508" y="164" width="19" height="24"/>
<rect class="cell ins" data-s="b" data-t="1" x="529" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="550" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="571" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="2" x="592" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="1" x="613" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="0" x="634" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="2" x="655" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="4" x="676" y="164" width="19" height="24"/>
<rect class="cell" data-s="b" data-t="1" x="697" y="164" width="19" height="24"/>
<path class="ln" data-s="p" data-t="4" d="M13.5 68 L13.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M34.5 68 L34.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M55.5 68 L55.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M76.5 68 L76.5 164"/>
<path class="ln" data-s="p" data-t="1" d="M97.5 68 L97.5 164"/>
<path class="ln" data-s="p" data-t="1" d="M118.5 68 L118.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M139.5 68 L139.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M160.5 68 L160.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M181.5 68 L244.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M202.5 68 L265.5 164"/>
<path class="ln" data-s="p" data-t="3" d="M223.5 68 L286.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M244.5 68 L307.5 164"/>
<path class="ln" data-s="p" data-t="1" d="M265.5 68 L328.5 164"/>
<path class="ln" data-s="p" data-t="1" d="M286.5 68 L349.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M328.5 68 L370.5 164"/>
<path class="ln" data-s="p" data-t="2" d="M349.5 68 L391.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M370.5 68 L412.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M391.5 68 L433.5 164"/>
<path class="ln" data-s="p" data-t="4" d="M412.5 68 L454.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M433.5 68 L475.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M454.5 68 L496.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M475.5 68 L517.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M517.5 68 L559.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M538.5 68 L580.5 164"/>
<path class="ln" data-s="p" data-t="2" d="M559.5 68 L601.5 164"/>
<path class="ln" data-s="p" data-t="1" d="M580.5 68 L622.5 164"/>
<path class="ln" data-s="p" data-t="0" d="M601.5 68 L643.5 164"/>
<path class="ln" data-s="p" data-t="2" d="M622.5 68 L664.5 164"/>
<path class="ln" data-s="p" data-t="4" d="M643.5 68 L685.5 164"/>
<path class="ln" data-s="p" data-t="1" d="M664.5 68 L706.5 164"/>
<text x="4" y="210" font-weight="700">B: 3 lines inserted after line 8, line 15 deleted, line 24 changed</text>
</svg>
<figcaption>Filled cells are the lines of stratum 2 (L = 2): their xxh3 hash ends in two zero bits. Green outlines mark lines only in B, red ones lines only in A. The hashes are those of real lines, rows such as <code>(20, 'Kathleen', 1960);</code>.</figcaption>
</figure>

| Stratum | Sampled in A | Sampled in B | Same line in both |
|---|---|---|---|
| 0 | 32 | 34 | 30 |
| 1 | 13 | 15 | 13 |
| 2 | 7 | 7 | 7 |
| 3 | 4 | 4 | 4 |
| 4 | 3 | 3 | 3 |

At stratum 1, B samples two lines that A does not: an inserted line and the changed line 24, which happen to hash to an even number. The lines that both files share are sampled in both or in neither, at every stratum.

Raising the stratum thins a sample in place. `strata::thin` drops the samples whose hash has a one among the new low bits; it never needs to read the file again. Lowering it would need the lines that were dropped, so strata only go up during a scan.

Sources: `crates/rift-core/src/strata/mod.rs`, `contains`, `thin` and `initial_level`.

## How the stratum follows from the budget

Rift picks a starting stratum before it reads a byte. `ScanConfig::for_pair` gives a sixteenth of the skeleton half to the two line indexes and the rest to samples. It prices a sample at 40 bytes: the 24-byte record and 16 bytes for the slot it will take in the anchor table. It guesses the line count from the file sizes at 64 bytes per line, a low figure for dumps and CSV exports, so the guess errs toward too many lines. The starting stratum is the smallest `L` for which the guessed line count, shifted right by `L`, fits the sample capacity.

At the default budget the skeleton half is 128 MiB, which holds 3,145,728 samples. For a pair of 10 GiB files the guess is 335,544,321 lines, and the smallest `L` that fits is 7. The unit test `level_halves_the_lines_until_they_fit` pins that case.

The guess can be wrong in either direction, and the scan corrects one of them. Its sample vector starts at the expected size and doubles as samples arrive, reserving each step from the budget first. When the vector reaches its cap, or the budget refuses to grow it, the scan raises the stratum by one and thins what it has. Checkpoints work the same way: when they fill up, the interval between them doubles. After both files are scanned, the skeleton thins them to one common stratum, and raises it again whenever the anchor table, the chain search or the comparison buffers do not fit what is left.

A guess that was too pessimistic is not corrected: strata only go up, so a file of long lines keeps sample capacity unused. The code comment on `PESSIMISTIC_BYTES_PER_LINE` puts it at about six strata for 3.5 KB lines.

You can watch the choice in `rift --summary`, which prints the stratum. On a 256 MiB pair from `rift-gen --seed 7 --size 256M --profile shuffled --rows csv` (3,350,294 lines in A), `--mem 64M` ran at stratum 4, `--mem 256M` at stratum 2, and `--mem 1G` at stratum 0, every line a sample. Measured 2026-11-07 on the Apple M1 above.

Sources: `scan/mod.rs`, `ScanConfig::for_pair`, `PESSIMISTIC_BYTES_PER_LINE`, `Shared::push_samples`, `Shared::push_checkpoints` and `Shared::grow`; `crates/rift-core/src/skeleton/mod.rs`, `build_watched`.

## What the scan holds

All of it comes from the skeleton half. While a file is scanned: the workers' buffers and two small staging vectors each. Afterwards: the samples and the checkpoints, which stay until the map is built. Once the map is built the samples go; the two indexes stay, next to the segment map, for the life of the diff.
