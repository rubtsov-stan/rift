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

It installs `rift` and its man page. There is no apt repository.

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

## Usage

```sh
rift a.sql b.sql > changes.patch     # unified diff, as diff -u prints it
patch a.sql changes.patch            # a.sql is now b.sql
rift -i a.sql b.sql                  # browse the differences (or rift tui)
rift --summary a.sql b.sql           # line counts and the segment map only
rift --mem 1G a.csv b.csv            # a larger heap limit
rift --stats a b > changes.patch     # also peak heap, time and approximate
                                     # regions, on standard error
rift --no-moves a b > changes.patch  # skip the search for moved blocks;
                                     # the diff is the same
```

Exit codes follow `diff`: 0 when the files are identical, 1 when they
differ, 2 on an error. The smallest `--mem` that works depends on the
files. Below it, Rift exits with 2 and a budget error that ends with a
limit to rerun with, such as `rerun with --mem 10333K`. It is not a
lower bound, and a run at that limit can still stop at a later stage and
name a higher one.

In the terminal view, `n` and `p` jump to the next and previous
difference, space and `b` page down and up, `g` or `:` goes to a line of
the first file, and `q` quits. A hunk Rift is still refining shows as a
pending row and fills in by itself. Approximate hunks are yellow and
marked with `~`.

`--summary` and `--stats` also list blocks of at least three lines that
appear in both files at different places. The terminal view marks both
ends of such a block, and `m` jumps from one end to the other. The
unified diff does not change: a moved block is still a deletion and an
insertion there.

## Key mode

```sh
rift --key csv:id a.csv b.csv           # CSV column by header name
rift --key csv:1 a.csv b.csv            # CSV column by position, from 1
rift --key json:user.id a.ndjson b.ndjson
rift --key copy:id a.sql b.sql          # a column of the pg_dump COPY blocks
```

With `--key`, Rift matches records by key and ignores row order. It
prints one line per key that differs, with tabs between the fields and
line numbers counted from 1:

```
~17	42	40      changed: line 42 in a, line 40 in b
-18	43	-       removed: only in a
+90	-	88      added: only in b
```

The key appears as the file has it, quotes included. A tab, line break
or backslash in a key prints as `\t`, `\n` or `\\`. In CSV, a record can
span lines inside quotes, and the header row counts as a record keyed by
its own cell. In a `pg_dump`, only the rows of `COPY` blocks that have the
column count, and the same key in two tables names two records. Blank
lines and line endings play no part in the comparison.

Rift compares a record's bytes before it calls the record unchanged. A
key that occurs twice in one file stops the run with exit code 2 and
lists the first few. Exit codes otherwise follow `diff`. A run can stop
after it has printed some changes, and then the output is incomplete, so
a script should check the exit code before it uses the output.

Key mode writes no temporary files. When the keys do not fit `--mem`,
Rift splits them by hash and reads both files once per part, so a
smaller budget means more passes over the files. Each pass prints its
changes sorted by line. A 1 GiB CSV pair takes two passes at the default
256 MiB; `--stats` reports the count. A record too long for the budget
stops the run with a budget error that names a `--mem` under which the
record fits.

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
  approximate in every output. The unified diff puts `approx` after the
  hunk header, where `diff -p` puts a function name and `patch` ignores
  it. The terminal view draws the region in yellow, and `--stats` counts
  it.

## What is not guaranteed

- The diff is not minimal. Like patience diff in `git`, it can show a
  change as larger than the smallest possible one.
- An approximate region is a whole replacement: correct, but coarse. It
  happens in large regions without any line unique in both files, such
  as long runs of identical or repetitive lines.
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

`rift git-diff` is git's external diff command. Use it for large files
only, so everything else keeps git's own diff:

```sh
echo '*.sql diff=rift' >> .gitattributes
echo '*.csv diff=rift' >> .gitattributes
git config diff.rift.command 'rift git-diff --mem 1G'
```

`git diff` then prints git's headers with the paths from git and a
unified diff from Rift. The command exits 0 when files differ, since git
treats any other status as a failure. Git runs the command through the
shell, so options go inside the quotes. Renames and copies work. Unlike
git's own output, hunk headers have no function name and a work tree
file's hash shows as `0000000`.

If a side is a Git LFS pointer, Rift reads the object from
`.git/lfs/objects` in place, after checking its size against the pointer.
A missing object stops the run and names its oid; fetch it with
`git lfs fetch`. Without LFS, git first copies each blob that is not in
the work tree to a temporary file: on a 10 GB file that is 10 GB of disk
and the time to write it.

For the terminal view, use `git difftool`:

```sh
git config difftool.rift.cmd 'rift -i "$LOCAL" "$REMOTE"'
git difftool -t rift
```

## MCP server

`rift-mcp` serves a diff to an MCP client over standard input and output,
so the client reads a few kilobytes about two huge files instead of the
files. It is a separate program, released next to `rift`. Install it with
its own shell installer:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/rubtsov-stan/rift/releases/latest/download/rift-mcp-installer.sh | sh
```

Or build it from the repository:

```sh
cargo build --release -p rift-mcp    # target/release/rift-mcp
```

Add it to the client's configuration:

```json
{
  "mcpServers": {
    "rift": { "command": "/path/to/rift-mcp" }
  }
}
```

Three tools, all taking the two paths `a` and `b` and an optional `mem`
(`64M` by default, at most `256M`):

- `summary`: line and byte counts, segments by status, the first 20
  differing ranges, and the memory the diff holds.
- `hunks_at(line, context?, max_lines?)`: the change in or next to a
  line of A, as unified diff text. Defaults are 3 lines of context and
  100 lines; the limits are 20 and 500. Text is also capped at 64 KiB
  and each line at 256 bytes.
- `next_difference(after_line)`: the first differing range that starts
  after a line of A. Pass a returned `a.start` to step forward.

The server opens regular files read-only. It refuses symbolic links,
directories, devices and pipes. It keeps the diff of each pair of files
between calls, so only the first call scans them. At most 4 diffs are
alive and their `mem` adds up to at most 512 MiB; a new pair evicts the
least recently used diff that no call is using, and a diff idle for 5
minutes is dropped. A changed file is a new pair.

## Benchmarks

Wall time and peak memory of Rift, `diff` and `git diff --no-index` on
the two pairs of generated SQL dumps from `bench/data.sh`, with Rift at
`--mem 256M`.

Apple M1, 16 GB RAM, internal SSD, macOS, warm page cache.

| Pair | Tool | Time | Peak memory |
|---|---|---|---|
| 1 GiB | `rift --summary` | 1.26 s | 127.7 MiB |
| 1 GiB | `rift` (full diff) | 0.93 s | 130.8 MiB |
| 1 GiB | system `diff` | 17.55 s | 3.2 GiB |
| 10 GiB | `rift --summary` | 13.37 s | 136.5 MiB |
| 10 GiB | `rift` (full diff) | 26.16 s | 136.5 MiB |
| 10 GiB | `diff` | did not run: needs more than 16 GB of RAM | |
| 10 GiB | `git diff` | did not run: needs more than 16 GB of RAM | |

Peak memory is the resident set; Rift's heap, the part `--mem` limits,
peaked at 83.5 MiB on the 1 GiB pair and 94.1 MiB on the 10 GiB pair.
`git diff` has no 1 GiB row because it treats files over 512 MiB as binary
and prints no diff.

Intel Core i5-10600 (6 cores, 12 threads), 16 GB RAM, Ubuntu 26.04.1,
Linux 7.0, bare metal.

`bench/linux-safe.sh` ran Rift v0.1.0, the version behind the macOS
table, on generated SQL pairs with the same seed. The files were in
tmpfs, so this run measures CPU and memory, not the disk. Each time is
the median of three runs.

| Pair | Tool | Time | Peak memory |
|---|---|---|---|
| 256 MiB | `rift` (full diff) | 0.28 s | 112.8 MiB |
| 256 MiB | GNU `diff` | 1.62 s | 776.3 MiB |
| 256 MiB | `git diff` | 2.57 s | 1.0 GiB |
| 1 GiB | `rift --summary` | 0.51 s | 113.0 MiB |
| 1 GiB | `rift` (full diff) | 0.69 s | 113.0 MiB |
| 1 GiB | GNU `diff` | killed at 3 GiB | |
| 2 GiB | `rift --summary` | 0.86 s | 113.0 MiB |
| 2 GiB | `rift` (full diff) | 1.20 s | 113.4 MiB |

The script capped `diff` at 3 GiB on the 1 GiB pair. The cap killed the
first run, and the script skipped the other two. At `--mem 256M`, Rift's heap
peaked between 83.2 and 83.6 MiB. The full report, CSV pairs and thread
scaling included, is in
[bench/results/linux-intel-core-i5-10600-2026-11-06](bench/results/linux-intel-core-i5-10600-2026-11-06/report.md).

`bench/run.sh` prints the full results with the commands and seeds, and
`bench/data.sh` regenerates the data.

## License

Rift is dual-licensed under MIT and Apache 2.0, like Rust itself, so you can use it under whichever fits your project. See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
