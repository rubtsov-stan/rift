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
10 GiB.

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

## Example

```sh
rift a.sql b.sql > changes.patch     # unified diff, as diff -u prints it
patch a.sql changes.patch            # a.sql is now b.sql
```

Exit codes follow `diff`: 0 when the files are identical, 1 when they
differ, 2 on an error.

## Documentation

The site has a guide to every mode (the terminal view, `--summary`, key
mode, moved blocks, `--mem`, exit codes), pages for common searches, how
Rift works, the benchmarks with their commands, and the man page:
<https://rubtsov-stan.github.io/rift/>. The design and its invariants are
in [DESIGN.md](DESIGN.md).

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

`rift git-diff` is git's external diff command, for files git would call
binary. It ships in 0.3.0. The
[git guide](https://rubtsov-stan.github.io/rift/guide/git/) has the
`.gitattributes` setup, the LFS behaviour and the cost of the temporary
copy git makes.

## MCP server

`rift-mcp` serves a diff to an MCP client, so the client reads a few
kilobytes about two huge files instead of the files. It is released next
to `rift`; see the
[MCP guide](https://rubtsov-stan.github.io/rift/guide/mcp/).

## VS Code

An extension shows the difference map and the moved blocks of two huge
files in a side panel and opens each difference in VS Code's diff editor,
on just that excerpt. It is built on `rift serve`, the same engine and
`--mem` limit as the command line. See the
[extension README](editors/vscode/README.md). It is not yet published: there
is no Marketplace page yet, only the source here.

## License

Rift is dual-licensed under MIT and Apache 2.0, like Rust itself, so you can use it under whichever fits your project. See [LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE).
