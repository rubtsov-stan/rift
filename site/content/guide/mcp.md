+++
title = "rift-mcp: let an MCP client read a diff of huge files"
description = "rift-mcp serves a diff to an MCP client over standard input and output, so the client reads a few kilobytes about two huge files instead of the files."
weight = 9
[extra]
nav = "MCP server"
h1 = "MCP server"
lead = "rift-mcp serves a diff to an MCP client, so the client reads a few kilobytes about two huge files instead of the files."
summary = "Install rift-mcp, add it to a client and use its three tools."
+++

`rift-mcp` is a separate program, released next to `rift`. It speaks MCP over standard input and output.

## Install

```
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/rubtsov-stan/rift/releases/latest/download/rift-mcp-installer.sh | sh
```

Or build it from the repository:

```
cargo build --release -p rift-mcp
```

The binary is `target/release/rift-mcp`.

## Add it to the client

```json
{
  "mcpServers": {
    "rift": { "command": "/path/to/rift-mcp" }
  }
}
```

## Tools

All three take the two paths `a` and `b` and an optional `mem` (`64M` by default, at most `256M`).

| Tool | What it returns |
|---|---|
| `summary` | Line and byte counts, segments by status, `refined_segments`, the first 20 differing ranges, and the memory the diff holds. `approx` counts the refined segments that came out approximate, `differs` the other differing segments. |
| `hunks_at(line, context?, max_lines?)` | The change in or next to a line of A, as unified diff text. Defaults are 3 lines of context and 100 lines; the limits are 20 and 500. Text is also capped at 64 KiB and each line at 256 bytes. |
| `next_difference(after_line)` | The first differing range that starts after a line of A. Pass a returned `a.start` to step forward. |

## What the server allows

- It opens regular files read-only, and refuses symbolic links, directories, devices and pipes.
- It keeps the diff of each pair of files between calls, so only the first call scans them. A changed file is a new pair.
- At most 4 diffs are alive, and their `mem` adds up to at most 512 MiB. A new pair evicts the least recently used diff that no call is using, and a diff idle for 5 minutes is dropped.
