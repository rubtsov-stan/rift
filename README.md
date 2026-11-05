<p align="center">
  <img src="docs/logo.svg" width="112" alt="">
</p>

<h1 align="center">Rift</h1>

<p align="center">
  Diff huge text files under a hard memory limit.
</p>

<p align="center">
  <a href="https://github.com/rubtsov-stan/rift/actions/workflows/ci.yml"><img src="https://img.shields.io/github/actions/workflow/status/rubtsov-stan/rift/ci.yml?branch=main&label=CI" alt="CI"></a>
  <a href="https://github.com/rubtsov-stan/rift/releases/latest"><img src="https://img.shields.io/github/v/release/rubtsov-stan/rift" alt="Release"></a>
</p>

<p align="center">
  <img src="docs/demo.gif" alt="rift --summary on a 64 MiB pair, then the terminal view jumping between differences" width="720">
</p>

Rift diffs text files of any size, from megabytes to hundreds of
gigabytes, under a heap limit you set on the command line (`--mem`,
256 MiB by default). It writes a unified diff that `patch` applies to the
first file to get the second, and has a terminal view that lets you jump
through the differences before the whole diff is done.

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

Wall time and peak memory of Rift, `diff` and `git diff --no-index` on
the two pairs of generated SQL dumps from `bench/data.sh`, with Rift at
`--mem 256M`.

Apple M1, 16 GB RAM, internal SSD, macOS, warm page cache.

| Pair | Tool | Time | Peak memory |
|---|---|---|---|
| 1 GB | `rift --summary` | 1.26 s | 127.7 MiB |
| 1 GB | `rift` (full diff) | 0.93 s | 130.8 MiB |
| 1 GB | system `diff` | 17.55 s | 3.2 GiB |
| 10 GB | `rift --summary` | 13.37 s | 136.5 MiB |
| 10 GB | `rift` (full diff) | 26.16 s | 136.5 MiB |
| 10 GB | `diff` | did not run: needs more than 16 GB of RAM | |
| 10 GB | `git diff` | did not run: needs more than 16 GB of RAM | |

Peak memory is the resident set; Rift's heap, the part `--mem` limits,
peaked at 83.5 MiB on the 1 GB pair and 94.1 MiB on the 10 GB pair.
`git diff` has no 1 GB row because it treats files over 512 MiB as binary
and prints no diff.

`bench/run.sh` prints the full results with the commands and seeds, and
`bench/data.sh` regenerates the data.

## License

Rift is dual-licensed under MIT and Apache 2.0, like Rust itself, so you can use it under whichever fits your project. See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
