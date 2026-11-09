+++
title = "MCP server to diff large files"
description = "rift-mcp lets an MCP client diff files of a gigabyte and more. The agent reads a few kilobytes of the diff, not the files. Setup for four clients."
weight = 9
[extra]
nav = "MCP server"
h1 = "MCP server"
lead = "rift-mcp serves a diff to an MCP client, so the client reads a few kilobytes about two huge files instead of the files."
summary = "What rift-mcp gives an agent, its three tools, a recorded session on a 1 GiB pair, the size of every answer and setup for four clients."
+++

`rift-mcp` is a separate program, released next to `rift`. It speaks MCP over standard input and output.

## What it gives an agent

An agent cannot read a 1 GiB file into its context, and it cannot read the output of `diff` on two of them either. With `rift-mcp` the agent asks questions and gets short answers:

- how many lines and bytes differ, and where the first differences are;
- the next difference after a line it already looked at;
- the lines of one change, as unified diff text, cut at a size it sets.

The server builds the diff of a pair of files once and keeps it for the next calls, so the first call scans the files and the rest answer from the kept diff. It opens files read-only. Every answer has an upper size, listed under "What bounds an answer" below.

`rift-mcp` diffs by line. `rift --key` is the CLI's keyed comparison, described in the [key mode guide](@/guide/key-mode.md).

## Install

```
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/rubtsov-stan/rift/releases/latest/download/rift-mcp-installer.sh | sh
```

Or build it from the repository:

```
cargo build --release -p rift-mcp
```

The binary is `target/release/rift-mcp`. Use its absolute path in the snippets below.

## Add it to a client

Each snippet follows the client's own documentation, checked on 12 November 2026. Replace `/path/to/rift-mcp` with the path of the binary.

### Claude Code

```
claude mcp add --transport stdio --scope user rift -- /path/to/rift-mcp
```

The `--` ends Claude Code's options and starts the server command. `--scope user` makes the server available in every project. Without it the server belongs to the current project. See [Claude Code: connect to tools with MCP](https://code.claude.com/docs/en/mcp).

### Claude Desktop

Open Settings, then Developer, then Edit Config. The file is `~/Library/Application Support/Claude/claude_desktop_config.json` on macOS and `%APPDATA%\Claude\claude_desktop_config.json` on Windows.

```json
{
  "mcpServers": {
    "rift": { "command": "/path/to/rift-mcp" }
  }
}
```

Quit Claude Desktop and start it again. See [Connect to local MCP servers](https://modelcontextprotocol.io/docs/develop/connect-local-servers).

### Cursor

Put this in `.cursor/mcp.json` in a project, or in `~/.cursor/mcp.json` for every project.

```json
{
  "mcpServers": {
    "rift": {
      "type": "stdio",
      "command": "/path/to/rift-mcp"
    }
  }
}
```

See [Cursor: Model Context Protocol](https://cursor.com/docs/mcp).

### VS Code

Put this in `.mcp.json` at the workspace root, or in `~/.copilot/mcp-config.json` for every workspace. VS Code reads the workspace-root file only when the setting `chat.mcp.workspaceRootConfig.enabled` is on. Set `COPILOT_HOME` to move the user file.

```json
{
  "mcpServers": {
    "rift": {
      "type": "stdio",
      "command": "/path/to/rift-mcp",
      "tools": ["*"]
    }
  }
}
```

`"tools": ["*"]` makes every tool of the server available. See VS Code's [MCP configuration reference](https://code.visualstudio.com/docs/agents/reference/mcp-configuration).

## Tools

The table comes from the server's own `tools/list` answer. A `?` marks an optional argument.

{{ mcp_tools() }}

`mem` is a memory budget like `--mem`: `64M` by default, at most `256M`. A live diff keeps the budget it was built with. In the answer of `summary`, `approx` counts the refined segments that came out approximate and `differs` counts the other differing segments. Its `mem_reserved_bytes` is the memory the session holds. That includes a response pool of 1/16 of the refine half, at most 8 MiB, which every session reserves before the diff is built. The MCP tools do not use it, so refinement gets that much less of `mem`.

## A session on a 1 GiB pair

A script, `site/scripts/record-mcp.py`, started `rift-mcp` and sent these calls over standard input and output. The questions are written for the recording, and the calls and answers are the real ones, unchanged except for the cuts. A line starting with `...` marks a cut, and an answer shows only its `structuredContent`. The server also sends the same JSON as text in `content`, so the byte count under each answer, which is that of the whole response line, covers both copies. The calls after the first take their line numbers from the answers before them, as an agent would. `mem_reserved_bytes` in the first answer differed by a few kilobytes between recordings of the same pair.

{{ mcp_session(id="sql") }}

## What bounds an answer

| Bound | Value |
|---|---|
| Ranges in a `summary` answer | The first 20. `more_differences` counts the rest. |
| Diff lines in a `hunks_at` answer | `max_lines`, 100 by default, from 1 to 500. A larger value is cut to 500. |
| Text of a `hunks_at` answer | 64 KiB. The text stops at the first line that starts after it has reached 64 KiB. |
| One diff line | 256 bytes. A longer line ends with `[+N bytes]`, the number of bytes cut. |
| Context lines | `context`, 3 by default, at most 20. |
| Ranges in a `next_difference` answer | One. |
| Wait for a hunk | 60 seconds. After that the call fails with "the hunk was not computed in time". |

A `hunks_at` answer says when it cut the hunk: `truncated` is `true` and `lines_shown` is smaller than `lines_total`.

## What the server allows

- It opens regular files read-only, and refuses symbolic links, directories, devices and pipes.
- It keeps the diff of each pair of files between calls, so only the first call scans them. A changed file is a new pair.
- `mem` is 64M unless the call sets it, and at most 256M for one diff. A larger value fails with an error, as the last call of the session above shows.
- At most 4 diffs are alive, and their `mem` adds up to at most 512 MiB. A new pair evicts the least recently used diff that no call is using, and a diff idle for 5 minutes is dropped.
