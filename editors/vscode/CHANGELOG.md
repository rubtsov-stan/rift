# Changelog

## [Unreleased]

## [0.3.3] - 2026-11-14

- Bundles `rift` 0.3.3.
- A move list the server reports as `partial` shows as "N moved, list full". A
  `moves` value the extension does not know is read as found when blocks are
  listed, so a newer server stays usable.
- Comparing the same pair again no longer closes the session it reuses, which made
  every hunk fail, and keeps the approx marks the first comparison had.
- A comparison that finishes late, after busy retries, no longer replaces a newer
  one, closes its session, or shows its failure. Once replaced, a compare sends
  nothing more, busy retries included, so it can no longer make the server drop
  the shown session. A session is closed only when neither the shown comparison
  nor the newest compare holds it, and never while that compare's open is on its
  way: the server matches files, not paths, so the same files under another path
  get the same session. Each attempt of an open, a retry after busy included,
  waits until the closes already sent are answered. A session id is tied to the
  server process that issued it, so nothing is sent for one after a restart, and a
  notice only counts from the process that issued the session. When the server no
  longer has the shown session, the extension says so instead of ignoring every
  click. A comparison that failed to load no longer keeps a new comparison of the
  same pair from replacing it. Closing the comparison also stops a compare that is
  still opening.
- A carriage return inside a line is shown as ␍ with a hover note. It used to split
  the line in the editor and shift the lines after it.
- A reply that is not JSON ends the server connection and fails the request with
  that reason. It left the request pending and raised an uncaught error. The next
  request starts the server again. The shown comparison is marked failed, since
  the new process numbers its sessions from 1 again; the stop is reported as a
  stop, not a crash. A compare that was still opening or loading is opened again
  on a new process.
  A restarted server that refuses to initialize fails the shown comparison with its
  error.
- A server that rejects `initialize` is stopped and started again on the next
  request, with the current `rift.path`. The error used to stay cached.
- A comparison that crashes the server twice without finishing its refinement in
  between is marked failed. The extension used to restart the server and reopen it
  without end. A stop for a reply that cannot be read counts the same way, and a
  compare given up on is shown failed and sends nothing more.
  A crash during a rerun reopens the pair with the memory the rerun asked for, and
  shows one message; it used to leave the pair failed.
- When a compare waits for a close the server has not answered within 10 s, the
  extension stops the server and opens the compare on a new process, instead of
  waiting for good.
- The summary row reads the refine progress from the notifications, so it no
  longer shows "refining 0/287" while the status bar is at 286/287.
- Segments already listed are re-read when the refinement ends, so with a server
  that reports refined approx in `segments` the segments never opened get their
  approx mark too, also when the last progress notice arrives together with the
  first page.
- A `rift:` document address now carries a per-window token, and one that is not
  loaded shows a note instead of another difference's lines or an empty editor.

## [0.3.2] - 2026-11-10

- Bundles `rift` 0.3.2, which refuses a pipe, FIFO or other non-regular
  file instead of reading it as empty.

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
