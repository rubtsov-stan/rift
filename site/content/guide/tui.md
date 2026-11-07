+++
title = "The terminal view: jump between differences in huge files"
description = "rift -i opens a terminal view you can use before the diff is complete. Jump with n and p, go to a line, follow moved blocks."
weight = 3
[extra]
nav = "Terminal view"
h1 = "The terminal view"
lead = "The terminal view lets you jump to the next difference while Rift is still working on the rest."
summary = "Browse the differences with n and p before the whole diff is done."
+++

```
rift -i a.sql b.sql
```

`rift tui a.sql b.sql` does the same. The view is usable before the diff is complete. A hunk Rift is still refining shows as one pending row and fills in by itself. Approximate hunks are yellow and marked with `~`.

{{ media(ready=true, mp4="demo.mp4", gif="demo.gif", poster="demo-poster.jpg", width=960, height=540, alt="rift --summary on a 64 MiB pair, then the terminal view jumping between differences") }}

## Keys

| Key | What it does |
|---|---|
| `n`, `p` | Jump to the next and previous difference. |
| `space`, `b` | Page down and up. |
| `g`, `:` | Go to a line of the first file. |
| `m` | Jump from the first moved block on screen to its other end. |
| `q` | Quit. |

## Limits

The view needs a terminal on standard input and standard output. When either is not a terminal, `rift -i` exits with status 2 before it reads the files. A build without the terminal interface (`cargo install --no-default-features`) also exits with status 2 on `-i`.

For git, use `git difftool` to open this view from a repository: see [git integration](@/guide/git.md).
