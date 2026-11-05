# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/rubtsov-stan/rift/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/rubtsov-stan/rift/releases/tag/v0.1.0
