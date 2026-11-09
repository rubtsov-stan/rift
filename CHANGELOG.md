# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- Key mode's output order depends on the files and `--mem` only. The
  same pair under the same limit prints the same bytes on every run, on
  any machine and any number of threads. The order at a given `--mem`
  can differ from 0.3.1's where key mode used several threads; the set
  of lines is the same.
- Key mode runs one shard per MiB of the refine half, from 1 to 8, on a
  thread pool of its own with one thread per shard, at most one per core
  or `RAYON_NUM_THREADS`. It uses no more than 8 threads on any machine.
- Key mode keeps 48 KiB of `--mem` out of the budget, without the 12 KiB
  per pool thread: it no longer starts the shared pool, and holds its
  own pool's heap, 16 KiB per shard, in the budget.

### Fixed

- `rift serve` could send `progress` or `session_state` for a session
  after it had answered that session's `close`. The notifier now lists
  the live sessions under the lock `close` answers under.
- `rift serve` could send a scan's last `progress` notification short of
  its total and then `session_state` ready, when the scan ended between
  the two reads. The notifier reads the state first.
- `rift serve` under a small `--max-total-mem` no longer advertises a
  `default_mem` or `max_session_mem` above what its sessions can hold.
  Every `open` used to answer `busy` forever; one that asks for more than
  the total now gets `too_large`. The server refuses to start when its
  limits leave a session less than 11 MiB, the least that runs even a
  one-line pair.
- Key mode's line order no longer changes between runs once it uses
  several threads.
- Key mode at `--mem 16M` runs on several threads again. The number of
  threads now grows with `--mem` instead of jumping from one at a refine
  half of 8 MiB. On the 128 MiB shuffled CSV pair it takes 1.09 s instead
  of 2.21 s in 0.3.1.

## [0.3.2] - 2026-11-10

### Fixed

- Earlier versions read FIFOs and process substitution (for example
  `rift <(cmd) file`) as empty files and printed a wrong diff with no
  error. Rift now refuses any operand that is not a regular file, with the
  path and a message to write the input to a file first. A directory names
  its path too, and `-` is documented as a file name.
- A Linux `/proc` file, which reports a length of 0 but has content, is
  refused the same way instead of being read as empty.
- A file cut while Rift reads it, or one that holds less than it reports,
  is an error naming the file. Key mode used to take the early end as the
  end of its records and print a wrong diff with no error.
- A file that grows while Rift reads it no longer yields bytes past the
  length taken at open, which could mix two versions of the file in one
  diff. Reads stop at that length.
- `rift serve` no longer hangs when a path is swapped for a FIFO between
  its check and its `open`. The open does not block, refuses a file that
  is not regular and never follows a symbolic link.
- `rift git-diff` no longer blocks on a FIFO operand while it looks for a
  Git LFS pointer.
- A source that returns short reads could make the line reader return the
  tail of a long line as a whole line instead of refusing it as too long.
  The reader now keeps the line's start across short reads.

### Known issues

- The two known issues of 0.3.1 remain in this release: key mode's line
  order can differ between runs once it uses several threads, and key mode
  at exactly `--mem 16M` runs on one thread. Both are fixed on `main` for
  the next release.

## [0.3.1] - 2026-11-09

### Added

- `rift --key --show-records` prints the records under each change: A's
  behind `  - `, B's behind `  + `, with the prefix on each line of a
  record that spans lines. Rift reads the records back from the files
  through a 64 KiB buffer taken from the `--mem` budget.
- `--stats` prints the peak heap after an error as well, and
  `rift git-diff --stats` prints it after the diff or the error.

### Changed

- `--mem` and `rift git-diff --mem` are at least 64K. Below that, parsing
  the command line alone took more heap than the limit. The reserve
  leaves key mode less room at a small `--mem`, so it takes more passes:
  at 1M a 128 MiB CSV pair takes 145 passes instead of 122 and runs about
  19% longer.
- With the reserve below, a run that only just fit a small limit can now
  stop with a budget error. `--summary` of a 32 MiB SQL dump at
  `--mem 1M` did on 8 threads. The `rerun with --mem N` it prints names a
  limit that gets past that point; here 1089K, which gives the same
  summary as before.
- An approximate hunk says why in plain words: the unified diff writes
  `@@ ... @@ approx: <reason>`, `--stats` counts the segments for each
  reason, the terminal view shows the reason in the status line when it
  fits, and `rift-mcp` adds it to the hunk header. `Hunk::approx` in
  `rift-core` is now an `Option<ApproxReason>`.

### Fixed

- Key mode at a small `--mem` peaked up to 10 KiB over the limit: its
  output buffers and other heap outside the budget went uncounted, and in
  the other modes so did the thread pool's heap. Rift now keeps 48 KiB
  plus 12 KiB per pool thread (one per core, or `RAYON_NUM_THREADS`) of
  `--mem` out of the budget for that heap, and
  `rerun with --mem N` adds it to its advice.
- Output closed early, as in `rift a b | head`, no longer prints an error.
  Rift exits 141 without a message, the status a shell reports for
  `diff` killed by SIGPIPE.
- `rift git-diff` exits 0 without a message when git's pager quits early,
  so `git diff` no longer reports "external diff died".
- A failed write to standard output says "write failed", not "read failed".
- Two files, or two regions, that share no line are no longer marked
  `approx`. Deleting one side and inserting the other is the smallest
  diff there, and Rift now proves it from the line hashes: a different
  hash means a different line. The script is the same as before.
- `--summary` no longer prints an `approx` line. It refines nothing, so
  its count was always 0 whatever the files.
- The `summary` of `rift serve` and `rift-mcp` counted `approx` segments
  by the map, which never marks one, so it read 0. `approx` now counts
  the refined segments that came out approximate, and `differs` the other
  differing segments; together they are still every differing segment.
  `rift-mcp` adds `refined_segments`; in `rift serve`, `progress.done` is
  that count.

### Known issues

- Key mode at exactly `--mem 16M` runs on one thread: the reserve takes
  the refine half just under the 8 MiB key mode needs for several. On a
  128 MiB CSV pair it takes 2.21 s instead of 1.25 s in 0.3.0, about 77%
  longer. `--mem 16528K` or more restores the old time.
- Key mode's line order can differ between runs once it uses several
  threads. The set of lines is always the same.

## [0.3.0] - 2026-11-08

### Added

- `rift git-diff`, the command git runs for an external diff. It takes
  the seven arguments git passes and prints a unified diff with git's
  headers, and reads a Git LFS object in place when a side is a pointer.
- `rift serve`, Rift's protocol on standard input and output, JSON-RPC 2.0
  framed as in LSP, for editors. `docs/protocol.md` specifies it.
- `rift-session`, the crate that keeps the live diffs shared by
  `rift serve` and `rift-mcp`.
- A VS Code extension in `editors/vscode`, built on `rift serve`.

### Changed

- `serve` and `git-diff` are subcommands, listed in `rift --help`, so a
  file named `serve` or `git-diff` must now be given as `./serve` or
  `./git-diff`.

## [0.2.0] - 2026-11-07

### Added

- Shingle anchors: where a region has no line that is unique in both
  files, Rift now anchors on short runs of lines, so fewer regions come
  out as approximate replacements.
- `--key SPEC` compares records by key and ignores row order. `SPEC` is
  `csv:N`, `csv:NAME`, `json:PATH` or `copy:NAME` (the COPY blocks of a
  `pg_dump` file). It prints added, removed and changed keys with their
  line numbers, compares record bytes before it calls a record unchanged,
  and stops with exit status 2 on a duplicate key. A larger `--mem` means
  fewer passes over the files.
- Moved blocks: `--summary` and `--stats` list blocks of at least three
  lines that appear in both files at different places, and the terminal
  view marks both ends of a block and jumps from one end to the other with
  `m`. The unified diff does not change. Detection starts from sampled
  lines and does not find every block.
- `--no-moves` skips the search for moved blocks. The unified diff is the
  same; `--summary`, `--stats` and the terminal view say that detection
  was off. It cannot be combined with `--key`.
- `rift-mcp`, an MCP server that lets a client explore the diff of two
  large files with `summary`, `hunks_at` and `next_difference`. It is
  released as its own archives for the four targets and with its own shell
  installer.
- The terminal interface is an optional `tui` feature, on by default. In a
  build without it, `-i` and `rift tui` exit with status 2.
- `.deb` packages for x86_64 and aarch64 Linux, attached to each release.
- A man page, `man/rift.1`, generated from the command-line definition
  with `cargo xtask man` and included in the `rift` release archives and
  the `.deb` packages.
- `bench/linux-safe.sh` runs the benchmark on a Linux host from RAM only,
  with memory caps, so it leaves the host and its disks alone.
- Fuzz targets for the line scanner, the key parsers, the pipeline, the
  unified writer, the terminal sanitizer and the MCP tools, with
  `fuzz/run-linux.sh` to run them on a Linux host.
- Release archives carry a build provenance attestation that
  `gh attestation verify` checks.
- CI runs `rift-core` tests under Miri, `cargo-deny` and `cargo-audit`,
  CodeQL and an OpenSSF Scorecard check. Dependabot watches cargo and
  GitHub Actions, and third-party actions are pinned by commit.

### Changed

- `--stats` and `--summary` print the peak heap as
  `peak heap X of Y (N bytes)`: the rounded figure for reading and the
  exact byte count for checking against `--mem`.
- In key mode, an error after some changes were printed now says on
  standard error that the output above is incomplete and must be
  discarded. Check the exit status before using key output.
- A budget error ends with a `--mem` to rerun with, in every mode:
  `refine budget exceeded: requested 5290144 bytes, 5242880 available;
  rerun with --mem 10333K`. It is not a lower bound: a smaller limit can
  take another path and succeed. A run at that limit can stop at a later
  stage and name a higher one. A diff whose refine half cannot hold one
  worker reports the whole amount at once. In key mode, a record too
  long for the budget is read to its end, and the limit named is one
  under which it fits.
- There is no fixed minimum `--mem`. The 0.1.0 notes said a budget below
  about 12 MiB is refused; the smallest budget that works depends on the
  files.
- Key-mode errors about a missing column or key name the file as well
  as the side: `a (data/a.csv) has no column "zz"`.
- Budget errors start in lowercase (`skeleton budget exceeded`), and an
  I/O error shows its text once instead of twice.
- Faster scanning, anchor selection, chain search, scheduling, unified
  output and key mode on large files, with the same output. DESIGN.md
  lists what changed and how it was measured.
- `release.yml` is generated by cargo-dist and changes only by upgrading
  cargo-dist and regenerating it; Dependabot no longer updates it.

### Fixed

- Header timestamps at the ends of the time range no longer overflow.
- `-i` and `rift tui` exit with status 2 and `-i needs a terminal` when
  standard input or output is not a terminal, instead of panicking.
- Key mode no longer reports the last CSV record as changed when a quote
  left open runs to the end of the file and the files differ only in the
  final newline or its line ending (`7dd49f5`).
- The same holds when such a file ends in blank lines that differ only in
  number or line endings (`b6b1de8`).
- Key mode on a `pg_dump` file with many tables no longer slows down and
  uses more memory with more threads: chunks parsed in parallel could
  register tables a sequential read never sees. The output was not
  affected (`3696ddd`).

### Known limits

- Speed on very low-entropy input (files of a few distinct short lines)
  is not yet characterised; memory stays within `--mem`.

## [0.1.0] - 2026-11-05

First release.

### Added

- `rift A B` writes a unified diff that `patch` applies to the first file
  to get the second. Lines are compared byte by byte, including line
  endings and a missing final newline. Lines longer than the read buffer
  are compared in pieces.
- `--mem` sets the heap limit (256M by default; K, M, G and T units). Every
  allocation on the data path is reserved from the budget, and files are
  read with `pread` into fixed buffers. CI fails when the peak heap of a
  1 GiB pair exceeds `--mem`. A budget below about 12 MiB is refused.
- Regions that do not fit the refinement budget are replaced as a whole and
  marked `approx` after the hunk header in the unified diff, in yellow with
  `~` in the terminal view, and counted by `--stats`.
- `--summary` prints line counts, the segment map and resource use instead
  of a diff, and counts the differing ranges past the first ten.
- `--stats` prints peak heap, time and approximate regions to standard
  error after a diff.
- `-i` (or `rift tui A B`) opens a terminal view: a minimap, a hunk view,
  `n` and `p` to jump between differences, space and `b` to page, `g` or
  `:` to go to a line of the first file, `q` to quit. Hunks still being
  refined show as pending rows and fill in by themselves.
- Exit codes follow `diff`: 0 for identical files, 1 for different files,
  2 on an error.
- `rift-gen` generates seeded file pairs with a manifest of the mutations.
- `bench/run.sh` compares Rift with `diff` and `git diff --no-index`;
  `bench/data.sh` regenerates the data sets.
- Release binaries for macOS and Linux, x86_64 and arm64, and a shell
  installer. The minimum supported Rust version for building from source
  is 1.97.

[Unreleased]: https://github.com/rubtsov-stan/rift/compare/v0.3.2...HEAD
[0.3.2]: https://github.com/rubtsov-stan/rift/compare/v0.3.1...v0.3.2
[0.3.1]: https://github.com/rubtsov-stan/rift/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/rubtsov-stan/rift/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/rubtsov-stan/rift/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/rubtsov-stan/rift/releases/tag/v0.1.0
