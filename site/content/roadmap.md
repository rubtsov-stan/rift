+++
title = "Rift roadmap: what is next, and what is not planned"
description = "What comes after Rift 0.3.2: a faster 0.4, directory diff, field-level key diff and key mode over MCP. No dates."
weight = 5
[extra]
nav = "Roadmap"
h1 = "Roadmap"
lead = "What comes after the current release. Nothing here has a date."
+++

## Shipped

- **0.3.2.** A pipe, FIFO or process substitution such as `rift <(cmd) file` is refused with a message to write the input to a file first; earlier versions read it as an empty file and printed a wrong diff. A file cut while Rift reads it is an error naming the file.
- **0.3.1.** `--show-records` prints the records of each change in [key mode](@/guide/key-mode.md). Two files, or two regions, that share no line get an exact diff and no `approx` mark, and an approximate hunk says why in plain words. The heap stays within `--mem`, which is now at least 64K, with a reserve for the heap no budget counts. A closed pipe, as in `rift a b | head`, ends without a message.
- **VS Code extension.** Compares two huge files from VS Code and opens each difference in VS Code's own diff editor. See the [VS Code guide](@/guide/vscode.md).
- **`rift serve`.** Rift's protocol for editors: JSON-RPC 2.0 on standard input and output, with a size bound on every answer. The extension runs it; [Clients and protocol](@/design/clients.md) explains how it works.
- **`rift git-diff`.** Git's external diff for files git calls binary, with Git LFS pointers resolved. See the [git guide](@/guide/git.md).

## Next: 0.4

- **Faster, same answers.** A review of 0.3 found places where Rift does more work than it has to. None of these changes alters the output or the memory limit, and each ships only if it is measurably faster on real data.
  - [Key mode](@/guide/key-mode.md) will stop rereading the input from the start on every pass.
  - Scanning will stop slowing down at small `--mem` values.
  - On a cold disk, Rift will ask the OS to read ahead the parts it is about to diff.
  - Refining a hunk will read larger blocks and stop copying each line.
  - `rift serve` will answer the VS Code extension with fewer reads.
- **Directory diff.** `rift old/ new/` compares two trees, such as `pg_dump -Fd` output or sharded exports, under the same `--mem`, even with millions of files. It lists added, removed and changed files and finds renames by content.
- **Which fields changed.** In key mode, a changed record will show the columns that differ (`price: 10 → 12`), with `--fields` to narrow the output.
- **Key mode over MCP**, so an agent can compare CSV and NDJSON files by key. See the [MCP guide](@/guide/mcp.md).
- **Large files from Source Control.** In VS Code, open a big tracked file's changes in Rift straight from the Source Control view. See the [VS Code guide](@/guide/vscode.md).

The order may change with feedback. If something here matters to you, or something is missing, open an issue.

## Not planned

- A diff of whole JSON documents as trees.
- Parquet and other binary formats.

The limits Rift has today are on the [landing page](@/_index.md) and in the README, under "What is not guaranteed".
