+++
title = "Rift roadmap: what is next, and what is not planned"
description = "What comes after Rift 0.4.1: a 0.4.2 with cold-disk read-ahead and tuned builds, then Source Control and directory diff in VS Code. No dates."
weight = 5
[extra]
nav = "Roadmap"
h1 = "Roadmap"
lead = "What comes after the current release. Nothing here has a date."
+++

## Shipped

- **0.4.1.** The scan reads with eight threads at any `--mem` a full diff runs under, where up to `--mem 64M` it used one, and the peak heap is lower on the reference pairs from `--mem 16M` to `1G`. Refinement hashes a segment where it read it, which makes a file of a few repeated lines 9 to 13% faster on two reference pairs. A key mode run that takes several passes starts its first pass from the share of keys the tables hold, which reads the files fewer times but may print the changes in another order; the order still depends only on the files and `--mem`. `rift serve` reads the bytes behind a hunk request once, and answers `changed` when a file was written over after it was opened, where a hunk could show wrong lines; the VS Code extension then compares the pair again. A directory run names the side that failed, `--show-fields` shows a change after the first 64 bytes of a value, and a refused `--mem` advises at least `10475K`. See the [changelog](https://github.com/rubtsov-stan/rift/blob/main/CHANGELOG.md).
- **0.4.0.** `rift old/ new/` compares two directory trees under one `--mem`, as `diff -r` prints them, and finds renamed files by content. See the [guide](@/guide/directories.md). In [key mode](@/guide/key-mode.md), `--show-fields` lists the fields that differ under a changed key (`price: 10 -> 12`), `--fields` compares only the fields you name, a `pg_dump` key prints with its table, and a file with duplicate keys always stops with the duplicate-key error, never a `--mem` refusal. `rift-mcp` compares CSV, NDJSON and `pg_dump` records by key. Each `.deb`, VSIX and `.mcpb` in a release has a `.sha256` file next to it.
- **0.3.4.** `rift-mcp` waits up to 5 seconds for room when four diffs are alive, and a busy answer says when to retry. Each release carries an MCP bundle, `rift-mcp-<version>.mcpb`, and `rift-mcp` is in the MCP Registry. See the [MCP guide](@/guide/mcp.md).
- **0.3.3.** The same files under the same `--mem` give the same output on any machine and any thread count. A refused `--mem` names one at least twice as large, so a few reruns reach a budget that works. Key mode always reports a duplicate key, and runs on several threads from `--mem 16M`. `rift git-diff` and the plain diff quote file names the way git and GNU diff do.
- **0.3.2.** A pipe, FIFO or process substitution such as `rift <(cmd) file` is refused with a message to write the input to a file first; earlier versions read it as an empty file and printed a wrong diff. A file cut while Rift reads it is an error naming the file.
- **0.3.1.** `--show-records` prints the records of each change in [key mode](@/guide/key-mode.md). Two files, or two regions, that share no line get an exact diff and no `approx` mark, and an approximate hunk says why in plain words. The heap stays within `--mem`, which is now at least 64K, with a reserve for the heap no budget counts. A closed pipe, as in `rift a b | head`, ends without a message.
- **VS Code extension.** Compares two huge files from VS Code and opens each difference in VS Code's own diff editor. See the [VS Code guide](@/guide/vscode.md).
- **`rift serve`.** Rift's protocol for editors: JSON-RPC 2.0 on standard input and output, with a size bound on every answer. The extension runs it; [Clients and protocol](@/design/clients.md) explains how it works.
- **`rift git-diff`.** Git's external diff for files git calls binary, with Git LFS pointers resolved. See the [git guide](@/guide/git.md).

## Next: 0.4.2

- **Faster, same answers.** Each change ships only if it is measurably faster on real data. None of these changes alters the memory limit, and the output stays the same.
  - On a cold disk, Rift will ask the OS to read ahead the parts it is about to diff ([#50](https://github.com/rubtsov-stan/rift/issues/50)).
  - A larger `--mem` that runs slower will be looked at ([#45](https://github.com/rubtsov-stan/rift/issues/45)).
  - The byte check after a refined segment will read larger blocks without copying each line ([#104](https://github.com/rubtsov-stan/rift/issues/104)).
  - Release builds already use link-time optimisation. Profile-guided optimisation will be added where it is measurably faster ([#117](https://github.com/rubtsov-stan/rift/issues/117)).

## Later: 0.4.x

- **Large files from Source Control.** In VS Code, open a big tracked file's changes in Rift straight from the Source Control view. See the [VS Code guide](@/guide/vscode.md).
- **Directory diff in VS Code.** Compare two folders from VS Code and see the changed files in a tree, through `rift serve`.
- **Refinement at 1 GiB.** On a file of a thousand repeated lines, refinement at 1 GiB takes six times as long as at 256 MiB ([#46](https://github.com/rubtsov-stan/rift/issues/46)).
- **Few distinct lines.** A file made mostly of a few repeated lines will get a real diff instead of one whole-file replacement ([#44](https://github.com/rubtsov-stan/rift/issues/44)).

The order may change with feedback. If something here matters to you, or something is missing, open an issue.

## Not planned

- A diff of whole JSON documents as trees.
- Parquet and other binary formats.

The limits Rift has today are on the [landing page](@/_index.md) and in the README, under "What is not guaranteed".
