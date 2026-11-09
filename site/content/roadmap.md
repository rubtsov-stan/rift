+++
title = "Rift roadmap: what is next, and what is not planned"
description = "What comes after Rift 0.3.1: 0.3.2, directory diff and field-level key diff. No dates."
weight = 5
[extra]
nav = "Roadmap"
h1 = "Roadmap"
lead = "What comes after the current release. Nothing here has a date."
+++

## Shipped

- **0.3.1.** `--show-records` prints the records of each change in [key mode](@/guide/key-mode.md). Two files, or two regions, that share no line get an exact diff and no `approx` mark, and an approximate hunk says why in plain words. The heap stays within `--mem`, which is now at least 64K, with a reserve for the heap no budget counts. A closed pipe, as in `rift a b | head`, ends without a message.
- **VS Code extension.** Compares two huge files from VS Code and opens each difference in VS Code's own diff editor. See the [VS Code guide](@/guide/vscode.md).
- **`rift serve`.** Rift's protocol for editors: JSON-RPC 2.0 on standard input and output, with a size bound on every answer. The extension runs it; [Clients and protocol](@/design/clients.md) explains how it works.
- **`rift git-diff`.** Git's external diff for files git calls binary, with Git LFS pointers resolved. See the [git guide](@/guide/git.md).

## Next

- **0.3.2.** Key mode prints its lines in the same order on every run and every machine for a given `--mem` and input, and runs at `--mem 16M` as fast as 0.3.0 did.
- **Directory diff.** `rift A/ B/` walks both trees in sorted order in bounded memory and diffs each pair of files under the same `--mem`, with a summary of added, removed and changed files.
- **Field-level key diff.** Planned for 0.4. In [key mode](@/guide/key-mode.md), name the fields that changed for a changed key: CSV columns, top-level NDJSON keys, COPY columns. Until then, `--show-records` prints both records of a changed key in full.
- **Exact diff for regions that share only repeated lines.** Today Rift can replace such a region whole and marks it `approx`; an alignment that counts only the line pairs that match would make it exact.
- **Homebrew tap.** `brew install` from a tap repository, added to the release installers once the tap and its token exist.

## Not planned

- A diff of whole JSON documents as trees.
- Parquet and other binary formats.

The limits Rift has today are on the [landing page](@/_index.md) and in the README, under "What is not guaranteed".
