# Changelog

## [0.3.1] - 2026-11-09

- Bundles `rift` 0.3.1.
- The summary row's approx count now counts the segments that came out
  approximate once refined. It read 0 before.

## [0.3.0] - 2026-11-08

First release.

- Rift view with a summary row, the differing segments and the moved blocks.
- Hunk diffs in VS Code's diff editor on read-only `rift:` documents, with
  the original line ranges in the tab title.
- Commands: Compare Files, Compare with Rift, Next and Previous Difference,
  Go to Line in File A, Close Comparison.
- Status bar progress for the scan and the refinement.
- A rerun offer when a scan does not fit its memory budget: the server's advice plus 25%, rounded up to a power of two and capped at the server's limit. After two reruns the memory guide replaces the button.
- One `rift serve` process per window, restarted when it crashes.
- Settings `rift.path`, `rift.mem` and `rift.contextLines`.
- Platform packages for darwin-arm64, darwin-x64, linux-x64 and linux-arm64,
  each with the `rift` binary.
