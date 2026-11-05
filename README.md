<p align="center">
  <img src="docs/logo.svg" width="96" alt="Rift">
</p>

# Rift

[![CI](https://github.com/rubtsov-stan/rift/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/rubtsov-stan/rift/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/rubtsov-stan/rift)](https://github.com/rubtsov-stan/rift/releases/latest)

Rift diffs text files of any size, from megabytes to hundreds of
gigabytes, under a heap limit you set on the command line (`--mem`,
256 MiB by default). It writes a unified diff that `patch` applies to the
first file to get the second, and has a terminal view that lets you jump
through the differences before the whole diff is done.

![rift --summary on a 64 MiB pair, then the terminal view jumping between differences](docs/demo.gif)

Use it on database dumps before and after a migration, CSV or NDJSON
exports, generated fixtures and large golden files: inputs where `diff`
or `git diff` run out of memory.

## How it works

Rift hashes every line once and keeps a content-defined sample of them:
a line is sampled when the low bits of its hash are zero, so both files
sample the same lines whatever was inserted or deleted elsewhere. Sampled
lines that occur once in each file are anchors, and the longest chain of
anchors in the same order in both files cuts the pair into segments. A
segment whose bytes are identical on both sides is equal, confirmed byte
by byte; the others are refined on demand with patience diff and Myers.
A segment too large for the refinement budget is sampled again, more
densely, inside the segment only. The design, the invariants and the
measurements behind them are in [DESIGN.md](DESIGN.md).

## Install

Each GitHub release carries binaries for macOS and Linux (x86_64 and
arm64) and a shell installer:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/rubtsov-stan/rift/releases/latest/download/rift-diff-installer.sh | sh
```

From source, with Rust 1.97 or later:

```sh
cargo install --git https://github.com/rubtsov-stan/rift rift-diff
```

The binary is called `rift`.

## Usage

```sh
rift a.sql b.sql > changes.patch     # unified diff, as diff -u prints it
patch a.sql changes.patch            # a.sql is now b.sql
rift -i a.sql b.sql                  # browse the differences (or rift tui)
rift --summary a.sql b.sql           # line counts and the segment map only
rift --mem 1G a.csv b.csv            # a larger heap limit
rift --stats a b > changes.patch     # also peak heap, time and approximate
                                     # regions, on standard error
```

Exit codes follow `diff`: 0 when the files are identical, 1 when they
differ, 2 on an error. A `--mem` below about 12 MiB is refused with a
budget error.

In the terminal view, `n` and `p` jump to the next and previous
difference, space and `b` page down and up, `g` (or `:`) goes to a line
of the first file, and `q` quits. A hunk still being refined shows as a
pending row and fills in by itself; an approximate one is drawn in
yellow and marked with `~`.

## What is guaranteed

- The output is a correct edit script: applied to the first file it
  gives exactly the second, byte for byte, line endings and a missing
  final newline included.
- A line is shown as unchanged only after its bytes were compared;
  hashes only propose matches.
- The heap stays at or under `--mem`. Every allocation on the data path
  is reserved from the budget first, files are read with `pread` into
  fixed buffers (no `mmap`), and the tests and CI check the peak with a
  counting allocator.
- A region Rift could not refine within the budget is marked approximate
  in every output: `approx` after the hunk header in the unified diff
  (where `diff -p` puts a function name, so `patch` ignores it), yellow
  rows in the terminal view, and a count with `--stats`.

## What is not guaranteed

- The diff is not minimal. Like patience diff in `git`, it can show a
  change as larger than the smallest possible one.
- An approximate region is a whole replacement: correct, but coarse. It
  happens in large regions without any line unique in both files, such
  as long runs of identical or repetitive lines.
- Rows that moved as a block (a dump in a different order) are a
  deletion and an insertion, as in any line-based diff.
- Only the heap is bounded by `--mem`. The resident set also holds the
  binary, thread stacks and whatever page cache the kernel counts
  against the process.
- Windows is not supported.

## Benchmarks

`bench/run.sh` compares Rift with `diff` and `git diff --no-index` and
prints the table below together with the hardware and the commands;
`bench/data.sh` regenerates the data sets.

#### Hardware

- CPU: Apple M1
- RAM: 16.0 GiB
- Disk: APPLE SSD AP0512Q
- OS: macOS 27.0.1
- rift: 0e837b0 release profile, no target-cpu=native
- diff: Apple diff (based on FreeBSD diff) (not GNU; run and labelled as such)
- git: git version 2.50.1 (Apple Git-155)
- Page cache: warm (no root: caches not dropped)

#### Seeds and commands

- gen1g: existing pair in /Users/stan/rift-data/gen1g (bench/data.sh uses seed 7)
- gen10g: existing pair in /Users/stan/rift-data/gen10g (bench/data.sh uses seed 7)
- summary: `rift --summary --mem 256M a b`; full diff: `rift --stats --mem 256M a b`
- diff: `diff a b` (GNU when labelled so); git: `git diff --no-index a b`
- timing: `/usr/bin/time -l COMMAND`, stdout redirected to a file (output column is its size)

#### Results

| Pair | Command | Cache | Wall | Peak RSS | Output | Peak heap | Status |
|---|---|---|---|---|---|---|---|
| gen1g | rift --summary --mem 256M | warm | 1.26 s | 127.7 MiB | 673 B | 83.5 MiB | exit 1 |
| gen1g | rift --stats --mem 256M | warm | 0.93 s | 130.8 MiB | 23.0 MiB | 83.5 MiB | exit 1 |
| gen1g | system diff (not GNU) | warm | 17.55 s | 3.2 GiB | 22.9 MiB |  | exit 1 |
| gen1g | git diff --no-index a b | warm | 63.92 s | 4.9 MiB | 189 B |  | exit 1 |
| gen10g | rift --summary --mem 256M | warm | 13.37 s | 136.5 MiB | 686 B | 94.1 MiB | exit 1 |
| gen10g | rift --stats --mem 256M | warm | 26.16 s | 136.5 MiB | 238.2 MiB | 94.1 MiB | exit 1 |
| gen10g | diff | | not run: needs more memory than available (20.0 GiB of input, 16.0 GiB RAM) | | | | |
| gen10g | git diff --no-index | | not run: needs more memory than available (20.0 GiB of input, 16.0 GiB RAM) | | | | |

Notes on this run: `git diff` treats files above its 512 MiB
`core.bigFileThreshold` as binary, so on the 1 GiB pair it printed only
"Binary files differ". The page cache was not dropped (no root), but the
10 GiB pair is twice the RAM, so each of its passes reads mostly from the
disk. `rift --summary` reads both files twice (scan, then comparison of
the equal stretches); `cat a b` over the same pair took 6.6 to 6.9 s.


## License

MIT or Apache-2.0, at your option.
