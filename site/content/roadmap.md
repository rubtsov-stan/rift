+++
title = "Rift roadmap: what is next, and what is not planned"
description = "What comes after Rift 0.3: directory diff and field-level key diff. No dates."
weight = 5
[extra]
nav = "Roadmap"
h1 = "Roadmap"
lead = "What comes after the current release. Nothing here has a date."
+++

{% vscode(on=false) %}
Rift 0.3.0 is the current release. This page lists work that is not in it.
{% end %}

{% vscode() %}
## Shipped

- **VS Code extension.** Compares two huge files from VS Code and opens each difference in VS Code's own diff editor. See the [VS Code guide](@/guide/vscode.md).
- **`rift serve`.** Rift's protocol for editors: JSON-RPC 2.0 on standard input and output, with a size bound on every answer. The extension runs it; [Clients and protocol](@/design/clients.md) explains how it works.
- **`rift git-diff`.** Git's external diff for files git calls binary, with Git LFS pointers resolved. See the [git guide](@/guide/git.md).
{% end %}

## After 0.3

- **Directory diff.** `rift A/ B/` walks both trees in sorted order in bounded memory and diffs each pair of files under the same `--mem`, with a summary of added, removed and changed files.
- **Field-level key diff.** In [key mode](@/guide/key-mode.md), name the fields that changed for a changed key: CSV columns, top-level NDJSON keys, COPY columns.
- **Homebrew tap.** `brew install` from a tap repository, added to the release installers once the tap and its token exist.

## Not planned

- A diff of whole JSON documents as trees.
- Parquet and other binary formats.

The limits Rift has today are on the [landing page](@/_index.md) and in the README, under "What is not guaranteed".
