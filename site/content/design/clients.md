+++
title = "Clients and protocol: rift serve and bounded answers"
description = "Why rift serve keeps a diff alive for a client, what rift-mcp shares with it, and how every answer stays inside the --mem of its session."
weight = 9
[extra]
nav = "Clients and protocol"
h1 = "Clients and protocol"
lead = "A client in another process asks about a diff many times while you read it. rift serve keeps the diff of a pair alive between those questions and answers each one with a page or an excerpt of known size."
summary = "rift serve, the session layer it shares with rift-mcp, excerpts instead of files, and the bound on every answer."
+++

`rift serve` shipped in 0.3, with the [VS Code extension](@/guide/vscode.md) as its first client. `rift-mcp` shipped in 0.2. The contract is [docs/protocol.md](https://github.com/rubtsov-stan/rift/blob/main/docs/protocol.md), with a JSON schema per message under `docs/protocol/`.

## Why rift serve exists

`rift a b` builds the segment map, writes a diff and exits. The terminal view keeps the map in the same process and refines what is on screen. An editor runs in another process and asks over time: how much differs, the next page of differences, the lines of this one. Running `rift` once per question would scan both files again each time, two passes over the disk for every click.

`rift serve` keeps the diff of a pair in a session, so the scan happens once and each later question costs a page or a few reads. It is a subcommand of the `rift` binary, so a client ships or finds one executable. It speaks JSON-RPC 2.0 on standard input and output with the `Content-Length` framing of LSP, which editor tooling already knows. The main thread reads requests and runs each one on a thread of its own, at most eight at once, so a slow `hunk` does not hold up a `summary`. One more thread writes every message.

Sources: `crates/rift-cli/src/serve/mod.rs`, module docs, `Server::start`, `MAX_IN_FLIGHT`; `crates/rift-cli/src/serve/frame.rs`, `read`.

{{ clients_diagram() }}

## Two front ends, one session layer

`rift-mcp` does not speak this protocol. It speaks MCP, has three tools of its own (`summary`, `hunks_at`, `next_difference`) and renders its own unified text. What it shares with `rift serve` is the crate below both, `rift-session`: one diff per pair of files, opened read-only after the same path checks, reused while neither file changes, bounded in count and in the sum of budgets.

A pair counts as unchanged while both files keep their device, inode, length and modification time. A symbolic link in the last path component, a directory, a device, a FIFO or a socket is refused before anything is read.

The two front ends use the layer differently. `rift serve` calls `Sessions::open`, which starts the scan on a thread and returns at once, and never drops a session for being idle. `rift-mcp` calls `Sessions::get`, which blocks until the scan is done, and keeps its own limits: 64 MiB per diff unless the call asks for more, at most 256 MiB per diff and 512 MiB in all, and diffs idle for five minutes dropped. Its MCP library runs on tokio, so it calls the layer from `spawn_blocking`.

The VS Code extension is a `rift serve` client. It starts one server per window, lists segments and moved blocks in a tree, and opens VS Code's own diff editor on the two excerpts of a segment.

Sources: `crates/rift-session/src/lib.rs`, crate docs; `sessions.rs`, `Sessions::open`, `Sessions::get`, `Limits::default`; `file.rs`, `FileId`, `FileSource::open`; `crates/rift-mcp/src/lib.rs`, `Server::session`; `crates/rift-mcp/src/main.rs`, `REAP_EVERY`.

## Excerpts, not files

A client never gets a whole file. An editor cannot open a 100 GiB file, and it does not need to: you look at one difference at a time. `hunk` returns the lines of one segment on both sides with up to 100 lines of context, read from the files with `pread`. It never waits for refinement, so a segment not yet refined answers as fast as a refined one, and asking for it queues it for refinement. The client diffs the two excerpts itself.

The excerpt carries text. Each line is decoded as UTF-8, with invalid bytes replaced and the line flagged `lossy`, and its terminator travels apart as `lf`, `crlf` or `none`. Every client shows text, and none applies a patch from an excerpt. An excerpt cut at the byte limit is not exact anyway. The byte-exact edit script stays in the output of `rift` itself.

Sources: `crates/rift-session/src/session.rs`, `Session::excerpt`, `column`; `docs/protocol.md`, "Line text and non-UTF-8 bytes".

## Every answer has a bound

The size of an answer does not depend on the size of the files. The bounds come from fixed caps and from the session's `--mem`:

- `segments` and `moves` return at most 1,000 entries a page, 100 by default. They borrow from the segment map and the block list the session holds anyway, and take no heap.
- `hunk` returns at most 5,000 lines per side and 65,536 bytes per line; the defaults are 1,000 lines and 4,096 bytes. A request above a cap is served at the cap, not refused.
- The lines and text of a `hunk` live in the session's **response pool**. When the scan starts, the session reserves a sixteenth of its Refine half for the pool, at most 8 MiB, before the diff sizes its workers and cache from what is left. At `--mem 256M` the pool is the full 8 MiB; at `--mem 64M` it is 2 MiB. A `hunk` stops at the line that would overflow its grant and reports `truncated`. Two requests that need more than the pool at once get `busy` for the later one. On a budget so small that one full-length line per side would not fit, lines are cut shorter instead.

The server itself keeps 8 MiB of `--max-total-mem` (default 2G) for the protocol and gives the rest to sessions. That reserve covers the input buffer and one request body of at most 64 KiB while it is parsed, the requests in flight, up to 16 messages waiting for the writer, and the writer's buffer. Each message is written through that fixed buffer after a first pass that only counts its length, so a large answer never exists as one string. A session asks for at most `--max-session-mem`, 1G by default, and the budgets of live sessions together stay within what is left.

Sources: `crates/rift-session/src/session.rs`, `MAX_PAGE`, `MAX_LINES`, `MAX_LINE_BYTES`, `DEFAULT_LINES`, `DEFAULT_LINE_BYTES`, `Session::excerpt`; `sessions.rs`, `Session::build`, `RESPONSE_SHARE`, `MAX_RESPONSE_POOL`; `crates/rift-cli/src/serve/mod.rs`, `PROTOCOL_RESERVE`, `QUEUE`, `IO_BUF`, `run`; `frame.rs`, `MAX_BODY`; `crates/rift-cli/src/lib.rs`, `ServeArgs`.

## Sessions and the scan in the background

`open` checks both paths and the budget, then answers at once with a session id in state `scanning`. The scan runs on its own thread. The core counts the bytes both passes have read in a `Watch`, out of twice the size of both files. One notifier thread looks at every live session each 50 ms and sends what changed: scan progress, the end of the scan in `session_state`, refinement progress, and `segment_ready` for the segments the client shows or asked for. A session gets at most 20 batches a second, however fast the work runs.

Until the scan ends, `summary` answers with the sizes and the progress, and `segments`, `hunk`, `moves` and `set_visible` answer `not_ready`, with the state and the progress in the error data. A scan that fails leaves a failed session that holds no budget and repeats its error through `summary`. A budget refusal carries the `--mem` to reopen with, computed by the same `BudgetExceeded::mem_advice` as the CLI's advice, and opening the pair again replaces the failed session.

When a new session does not fit four sessions or the memory left, the server drops failed sessions first, then the least recently used ready ones that no request is using. It never drops a scanning session; it answers `busy`.

`close` takes the session out of the table at once, so later requests get `unknown_session` and no notification names it again. On a scanning session it also cancels the `Watch`. The scan checks it before every read, stops when the reads under way end (at most one chunk per scan thread in the first pass, one comparison buffer per lane in the second) and releases the session's memory then. When standard input closes, the server cancels what runs, closes every session and exits with status 0.

Sources: `crates/rift-session/src/sessions.rs`, `Sessions::open`, `Sessions::close`, `Sessions::make_room`, `Session::scan_progress`; `crates/rift-core/src/scan/mod.rs`, `Watch`; `crates/rift-cli/src/serve/notify.rs`, `TICK`, `Server::notify`; `crates/rift-cli/src/serve/methods.rs`, `Server::ready`; `crates/rift-core/src/budget.rs`, `BudgetExceeded::mem_advice`.

<!-- Verified: rift serve sets Limits.idle to Duration::MAX, so make_room's idle pass never drops a session there; docs/protocol.md lists failed before idle, the code checks idle first. -->
