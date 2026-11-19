+++
title = "MCP server to diff large files"
description = "rift-mcp lets an MCP client diff files of a gigabyte and more. The agent reads a few kilobytes of the diff, not the files. Setup for four clients."
weight = 9
[extra]
nav = "MCP server"
h1 = "MCP server"
lead = "rift-mcp serves a diff to an MCP client, so the client reads a few kilobytes about two huge files instead of the files."
summary = "What rift-mcp gives an agent, its five tools, recorded sessions on a 1 GiB pair and a 32 MiB keyed pair, the size of every answer and setup for four clients."
+++

`rift-mcp` is a separate program, released next to `rift`. It speaks MCP over standard input and output.

## What it gives an agent

An agent cannot read a 1 GiB file into its context, and it cannot read the output of `diff` on two of them either. With `rift-mcp` the agent asks questions and gets short answers:

- how many lines and bytes differ, and where the first differences are;
- the next difference after a line it already looked at;
- the lines of one change, as unified diff text, cut at a size it sets;
- for files of records, which keys were added, removed or changed whatever the row order, and a page of the changed keys with their records and the fields that differ.

The server builds the diff of a pair of files once and keeps it for the next calls, so the first call scans the files and the rest answer from the kept diff. The keyed tools keep nothing: each call reads both files again. The server opens files read-only. Every answer has an upper size, listed under "What bounds an answer" below.

`summary`, `hunks_at` and `next_difference` diff by line. `keyed_summary` and `keyed_changes` are `rift --key` for a client, described in the [key mode guide](@/guide/key-mode.md).

## Install

```
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/rubtsov-stan/rift/releases/latest/download/rift-mcp-installer.sh | sh
```

`rift-mcp` is listed in the [MCP Registry](https://registry.modelcontextprotocol.io) as `io.github.rubtsov-stan/rift-mcp`.

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

#### Install the bundle

Each release carries `rift-mcp-<version>.mcpb`, a bundle of `rift-mcp` for macOS and for Linux on x86_64 and aarch64. Windows is not in it yet. Download the file from the [latest release](https://github.com/rubtsov-stan/rift/releases/latest), open Claude Desktop's Settings, then Extensions, and drag the file onto the window. Claude Desktop installs it and starts the server; no path to set.

The bundle is not signed yet. Its provenance can be checked with the file next to it in the release:

```
gh attestation verify rift-mcp-<version>.mcpb --bundle rift-mcp-<version>.mcpb.sigstore.json --repo rubtsov-stan/rift
```

Its checksum is in `rift-mcp-<version>.mcpb.sha256`, from 0.4.0 on: `shasum -a 256 -c rift-mcp-<version>.mcpb.sha256`, or `sha256sum -c` on Linux.

#### Or edit the config

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

`mem` is a memory budget like `--mem`: `64M` by default, at most `256M`. A live diff keeps the budget it was built with. In the answer of `summary`, `approx` counts the refined segments that came out approximate and `differs` counts the other differing segments.

## Key mode tools

`key` is the CLI's `--key`: `csv:NAME` for a header name, `csv:N` for column N counted from 1, `json:PATH` for an NDJSON field (dotted for a nested one) or `copy:NAME` for a column of the `COPY` blocks of a `pg_dump`. The CLI's [rules for keys](@/guide/key-mode.md) hold.

`keyed_summary` answers with `added`, `removed` and `changed`, the record counts of both files, the number of `passes`, and `first_added`, `first_removed` and `first_changed`. Each listed key is `{table, key, key_truncated, a_line, b_line}`.

`keyed_changes` answers with one page. Each change has `kind` (`added`, `removed` or `changed`), `table`, `key`, `a_line` and `b_line`, `a_record` and `b_record`, and, for a changed key, `fields`: the fields that differ as `{name, a, b, truncated}`, with `null` for a value one record lacks. `limit` is the most changes to return. The answer's `next` is the `cursor` of the next page, `null` after the last; the first call has no `cursor`. `fields` in the call limits the comparison to the named fields, as `--fields` does; an empty list means all fields.

A key from a `pg_dump` is not written as `public.item:7`. Its table is the field `table`, as the `COPY` header names it, with no escaping, and `key` holds the key alone. `table` is `null` for CSV and NDJSON. A key and a table keep the bytes the file has, so a CSV key keeps its quotes.

Nothing is kept between keyed calls. Every call, each page of `keyed_changes` included, reruns the whole comparison over both files, so the cost of a page is that of a complete comparison, and the comparison also reads and splits the records of every change before the page. A long list of changes is that many comparisons. A duplicate key fails the call wherever the page starts, and the counts cover all changes. Keeping the result between pages is [#89](https://github.com/rubtsov-stan/rift/issues/89).

A cursor is opaque. It holds the position in the comparison's order and a fingerprint of `key`, `mem`, `fields` and both files (device, inode, size and modification time). A call with a cursor from another call is refused with "cursor: the files or arguments changed since that cursor was issued; start again without a cursor", and so is a cursor that was cut or edited. `limit` may change between pages. The fingerprint only guards paging against a wrong page; it never decides that two records are equal.

A keyed call sets 128 KiB of its `mem` aside for the answer, so `mem` has a floor. Measured with `rift-mcp` over standard input and output, by bisection in steps of 1 KiB, on an Apple M1 (the figures do not vary between runs, as the budget is exact): a CSV pair of three rows needed 260K for `keyed_summary` and 274K for `keyed_changes`, and a 32 MiB CSV pair from `rift-gen --seed 7 --profile shuffled --rows csv` needed 369K and 398K. Below the floor the call fails with the budget error and a `--mem` to rerun with. The floor grows with the number of keys that wait for their match, so a pair that is far out of order needs more.

Errors come back as tool errors in the CLI's words: `a (/path/a.csv) has no column "price"`, `duplicate keys: public.item:7`, and for a budget too small `refine budget exceeded: requested ... bytes, ... available; rerun with --mem 10333K`. The `mem` of the call is that value.

The ranges in `first_differences` and in a `next_difference` answer come from the segment map before refinement. A range can take in equal lines next to a change, so a change on line 1000 can show as lines 999 to 1000. `hunks_at` refines the range and gives the exact lines.

## A session on a 1 GiB pair

A script, `site/scripts/record-mcp.py`, started `rift-mcp` and sent these calls over standard input and output. The questions are written for the recording, and the calls and answers are the real ones, unchanged except for the cuts. A line starting with `...` marks a cut, and an answer shows only its `structuredContent`. The server also sends the same JSON as text in `content`, so the byte count under each answer, which is that of the whole response line, covers both copies. The calls after the first take their line numbers from the answers before them, as an agent would. `mem_reserved_bytes` in the first answer differed by a few kilobytes between recordings of the same pair.

{{ mcp_session(id="sql") }}

## A keyed session on a 32 MiB pair

The same script recorded a second session on a CSV pair with the same keys in different orders. The last call asks for a key that is not a column.

{{ mcp_session(id="keyed") }}

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
| Keys in a `keyed_summary` answer | The first 10 added, 10 removed and 10 changed. The counts cover all. |
| Changes in a `keyed_changes` answer | `limit`, 20 by default, from 1 to 100. A larger value is cut to 100. |
| Text of a `keyed_changes` answer | 64 KiB of JSON text, counting each change as 256 bytes more than its text and each field as 64 bytes more. A change that would cross it starts the next page, unless it is the first of its page. |
| A key, table, field name or field value | 256 bytes, cut on a character boundary. `key_truncated` (for a key or its table) or `truncated` says so. |
| A record | 1024 bytes. `records_truncated` says so. |
| Fields listed for one changed key | 32. `fields_more` counts the rest. |
| A `cursor` | 50 characters. A longer or malformed one is refused. |
| `fields` in a call | At most 100 names of at most 256 bytes. |
| One change | If the first change of a page alone weighs more than 64 KiB, its field list is cut until it fits, and `fields_more` counts the fields left out. |
| Smallest `mem` of a keyed call | 128 KiB for the answer, on top of what the comparison needs: 260K to 398K in the measurements above. |

A `hunks_at` answer says when it cut the hunk: `truncated` is `true` and `lines_shown` is smaller than `lines_total`. A `keyed_changes` answer has a `next` cursor when a page ends before the last change, whether `limit` or the text bound ended it.

## What the server allows

- It opens regular files read-only, and refuses symbolic links, directories, devices and pipes.
- It keeps the diff of each pair of files between calls, so only the first call scans them. A changed file is a new pair.
- `mem` is 64M unless the call sets it, and at most 256M for one diff. A larger value fails with an error, as the last call of the session above shows.
- A keyed call takes the same `mem` limits: 64M unless the call sets it, at most 256M. Its `mem` counts in the 512 MiB total and the call takes one of the 4 slots until it returns, then gives both back. It evicts an idle diff for room like a new pair does, and waits and fails the same way.
- Of that `mem`, a keyed call sets 128 KiB of the refine half aside for the answer. A `mem` too small for that fails with the budget error and a `--mem` to rerun with.
- At most 4 diffs are alive, and their `mem` adds up to at most 512 MiB. A new pair evicts the least recently used diff that no call is using, and a diff idle for 5 minutes is dropped.
- A new pair that finds no diff to evict waits up to 5 seconds for one: a scan to end or a call to finish. At most 4 calls wait at once. A call past that, or one still without room after 5 seconds, fails with "the server is busy with other diffs; retry in N ms". N is when the scan nearest its end should finish, judged by its speed so far, or 5000 when no scan is running.
