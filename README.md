<p align="center">
  <img src="docs/logo.svg" width="112" alt="">
</p>

<h1 align="center">Rift</h1>

<p align="center">
  Diff huge text files under a hard memory limit.
</p>

<p align="center">
  <a href="https://github.com/rubtsov-stan/rift/actions/workflows/ci.yml"><img src="https://github.com/rubtsov-stan/rift/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="https://github.com/rubtsov-stan/rift/releases/latest"><img src="https://img.shields.io/github/v/release/rubtsov-stan/rift" alt="Release"></a>
  <a href="https://scorecard.dev/viewer/?uri=github.com/rubtsov-stan/rift"><img src="https://api.scorecard.dev/projects/github.com/rubtsov-stan/rift/badge" alt="OpenSSF Scorecard"></a>
</p>

<p align="center">
  <img src="docs/demo.gif" alt="rift --summary on a 64 MiB pair, then the terminal view jumping between differences" width="720">
</p>

Rift diffs large text files under a heap limit you set on the command
line (`--mem`, 256 MiB by default). It writes a unified diff that `patch`
applies to the first file to get the second, and has a terminal view that
lets you jump through the differences before the whole diff is done.

Use it on database dumps before and after a migration, CSV or NDJSON
exports, generated fixtures and large golden files: inputs where `diff`
or `git diff` run out of memory. The largest pair measured so far is
50 GiB.

| Pair, each of A and B | Tool | Time | Peak memory |
|---|---|---|---|
| 256 MiB | `rift --mem 256M` | 0.21 s | 108 MiB |
| 256 MiB | GNU `diff` | 1.62 s | 776 MiB |
| 256 MiB | `git diff --no-index` | 2.58 s | 1.0 GiB |
| 1 GiB | `rift --mem 256M` | 0.53 s | 109 MiB |
| 1 GiB | GNU `diff` | killed at a 3 GiB cap | |
| 1 GiB | `git diff --no-index` | 42.66 s, no diff (binary) | 5 MiB |
| 50 GiB, on a SATA SSD | `cat a b` (the disk's rate) | 218.7 s, 491 MB/s | 8 MiB |
| 50 GiB, on a SATA SSD | `rift --summary --mem 256M` | 299.8 s, 358 MB/s | 107 MiB |
| 50 GiB, on a SATA SSD | `rift --mem 256M`, diff output 1.2 GiB | 456.8 s, 235 MB/s | 113 MiB |

Generated SQL pairs (`rift-gen --seed 7`, 0.1% of lines changed). Intel
i5-10600, 16 GB RAM, Ubuntu 26.04, governor `powersave` (intel_pstate),
Rift at `94bdbb1`, 2026-11-14. The 256 MiB and 1 GiB pairs were in tmpfs
(medians of five runs). The 50 GiB pair was read from a Samsung SSD 860
(one run each, page cache not dropped, the pair 6.7 times the RAM), so
that run is disk-bound. Peak memory is the resident set; Rift's heap
stayed under 81 MiB in every row. Command: `bench/linux-safe.sh --suite
standard --reps 5 --ref main --disk-dir DIR`. Source:
`bench/results/linux-intel-core-i5-10600-2026-11-14/`. More rows and
the Apple M1 table are on the [benchmarks page](https://rubtsov-stan.github.io/rift/benchmarks/).

## Install

Each GitHub release has binaries for macOS and Linux on x86_64 and arm64,
and a shell installer:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/rubtsov-stan/rift/releases/latest/download/rift-diff-installer.sh | sh
```

On Debian and Ubuntu, download the `.deb` for your architecture (`amd64` or
`arm64`) from the release page and install it with apt, which also resolves
its dependencies:

```sh
sudo apt install ./rift-diff_VERSION-1_amd64.deb
```

It installs `rift` and its man page. There is no apt repository. Each
`.deb` has a checksum file next to it on the release page:

```sh
sha256sum -c rift-diff_VERSION-1_amd64.deb.sha256
```

From source, with Rust 1.97 or later:

```sh
cargo install --git https://github.com/rubtsov-stan/rift rift-diff
```

Add `--no-default-features` to build without the terminal interface; `-i`
and `rift tui` then exit with status 2.

Each release archive carries a build provenance attestation. To check
one you downloaded, with the [GitHub CLI](https://cli.github.com):

```sh
gh attestation verify rift-diff-x86_64-unknown-linux-gnu.tar.xz --repo rubtsov-stan/rift
```

The binary is called `rift`.

## Example

```sh
rift a.sql b.sql > changes.patch     # unified diff, as diff -u prints it
patch a.sql changes.patch            # a.sql is now b.sql
```

Exit codes follow `diff`: 0 when the files are identical, 1 when they
differ, 2 on an error. Both operands must be regular files, or both
directories (see below); symbolic links given as operands are followed.
Rift reads at offsets, so a pipe, a FIFO or `<(command)` is
refused with exit 2. Write the input to a file first. `-` is a file name, not
standard input.

## Directories

New in 0.4. Given two directories, Rift compares the trees and prints
what `diff -r` prints: `Only in` lines for files in one tree, a unified
diff under a `diff -r A/x B/x` header for each changed file, and
`Binary files ... differ` for a file with a NUL byte in its first 8 KiB.
A file only in A and a file only in B with the same bytes are one
`Renamed:` line. Symbolic links are compared by target, not followed.
`--summary` prints one status per path (A, D, M, R, T) and the counts.

```sh
rift old-dump/ new-dump/ > changes.diff
```

The whole tree runs under one `--mem`, one directory at a time: a tree
can hold millions of files, but one directory's listing must fit in a
quarter of the limit, 8 bytes per entry plus its name, on each side. At
the default `256M` that is about 2 million entries with 8-byte names in
a directory both trees have. A directory that does not fit is refused
with a limit to rerun with. Files that change during the run are not
detected. For `pg_dump -Fd`, dump with `-Z0` (`--compress=0`), or every
table shows as `Binary files ... differ`; reading the `.gz` files is
[#73](https://github.com/rubtsov-stan/rift/issues/73).

## Documentation

The site has a guide to every mode (the terminal view, `--summary`, key
mode, moved blocks, `--mem`, exit codes), pages for common searches, how
Rift works, the benchmarks with their commands, and the man page:
<https://rubtsov-stan.github.io/rift/>. The design and its invariants are
in [DESIGN.md](DESIGN.md).

What's next: <https://rubtsov-stan.github.io/rift/roadmap/>

## What is guaranteed

- The output is a correct edit script: applied to the first file it
  gives exactly the second, byte for byte, line endings and a missing
  final newline included.
- Rift shows a line as unchanged only after comparing its bytes; hashes
  only propose matches.
- The heap stays at or under `--mem`. Rift reserves every allocation on
  the data path from the budget first and reads files with `pread` into
  fixed buffers, never `mmap`. The tests and CI check the peak with a
  counting allocator.
- Rift marks a region it could not refine within the budget as
  approximate in every output. The unified diff puts `approx:` and the
  reason after the hunk header, where `diff -p` puts a function name and
  `patch` ignores it. The terminal view draws the region in yellow, and
  `--stats` counts it by reason.

## What is not guaranteed

- The diff is not minimal. Like patience diff in `git`, it can show a
  change as larger than the smallest possible one.
- An approximate region is a whole replacement: correct, but coarse. It
  happens in large regions without any line unique in both files, such
  as long runs of identical or repetitive lines. Files that share no
  line are replaced whole too, but that is the smallest diff, so it is
  not approximate.
- A file made mostly of a few repeated lines (about ten distinct lines
  or fewer) can come out as one whole-file replacement. The diff still
  applies, but it is far from minimal: on a 64 MiB file of one repeated
  line with 0.1% of lines changed, Rift printed 3.6 million changed
  lines where GNU diff printed 20,602. With ten distinct lines a 256 MiB
  pair came out exact and a 1 GiB pair whole; from 100 distinct lines up,
  Rift's counts were within 5% of GNU diff's on 64 MiB pairs.
  (`rift-gen` low-entropy profiles, seed 7, `rift --stats --mem 256M`,
  Intel i5-10600, Linux, 2026-11-14.)
- In the unified diff, a block of rows that moved, such as a dump
  written in a different order, shows as a deletion and an insertion, as
  in any line-based diff. `--summary`, `--stats` and the terminal view
  name moved blocks, but only those that contain a sampled line, so they
  can miss some.
- Only the heap is bounded by `--mem`. The resident set also holds the
  binary, thread stacks and whatever page cache the kernel counts
  against the process.
- Windows is not supported.

## Git

`rift git-diff` is git's external diff command, for files git would call
binary. It ships in 0.3.0. The
[git guide](https://rubtsov-stan.github.io/rift/guide/git/) has the
`.gitattributes` setup, the LFS behaviour and the cost of the temporary
copy git makes.

## MCP server

`rift-mcp` serves a diff to an MCP client, so the client reads a few
kilobytes about two huge files instead of the files. Besides the line
diff it compares CSV, NDJSON and `pg_dump` records by key: counts of
added, removed and changed keys, and pages of changes with the fields
that differ. It is released next to `rift`; see the
[MCP guide](https://rubtsov-stan.github.io/rift/guide/mcp/).

## VS Code

An extension shows the difference map and the moved blocks of two huge
files in a side panel and opens each difference in VS Code's diff editor,
on just that excerpt. It is built on `rift serve`, the same engine and
`--mem` limit as the command line. See the
[extension README](editors/vscode/README.md). Install it from the
[Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=rubtsov-stan.rift-diff)
or [Open VSX](https://open-vsx.org/extension/rubtsov-stan/rift-diff).

## License

Rift is dual-licensed under MIT and Apache 2.0, like Rust itself, so you can use it under whichever fits your project. See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
