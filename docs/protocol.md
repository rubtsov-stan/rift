# Rift protocol

`rift serve` speaks JSON-RPC 2.0 on standard input and output. The VS Code
extension, a future GUI and, later, `rift-mcp` are clients of it; the
session logic behind it lives in the `rift-session` crate. This document
is the contract: the server and its clients follow it, and the JSON
schemas under [`protocol/`](protocol/) describe every message in it.
`editors/vscode/test/mock-server.js` answers it from a fixed fixture,
and `node editors/vscode/test/check-mock.js` checks the schemas, the
fixture and the mock against each other.

Protocol version: **1**.

## Transport

- Framing as in LSP: each message is a header block, a blank line, then
  the body. The only header read is `Content-Length`, the body's length
  in bytes; others are ignored. Lines of the header end in `\r\n`.

  ```
  Content-Length: 52\r\n
  \r\n
  {"jsonrpc":"2.0","id":1,"method":"summary","params":{"session":1}}
  ```

- Bodies are UTF-8 JSON objects. Batches (JSON arrays) are not
  supported and get an `invalid_request` error.
- The client sends requests and the `$/cancelRequest` notification; the
  server sends responses and the `progress`, `session_state` and
  `segment_ready` notifications. The server never sends requests.
- Requests may be answered out of order. A slow request does not hold
  up the others.
- When standard input closes, the server cancels what is running, drops
  every session and exits with status 0.

## Conventions

- **Lines** are 1-based. A range is `{"start": s, "count": n}`: lines `s`
  to `s + n - 1`. An empty range (`count` 0) sits before line `start`,
  so a pure insertion after line 7 of A has the A range
  `{"start": 8, "count": 0}`. These are the ranges `rift-mcp` reports.
- **Sessions** are positive integers the server assigns in `open`. One
  session is one diff of one pair of files.
- **Bytes** are integers. Every count and size in the protocol stays
  below 2^53, so a JavaScript number holds it exactly.
- **Optional booleans** that would be `false` are left out.
- **Defaults and caps.** A request that asks for more than a cap is
  served at the cap, not refused; the response says what was left out.
  The caps are the numbers `initialize` returns under `limits`.

## Methods

| Method | Params | Result |
|---|---|---|
| `initialize` | `protocol`, `client?` | `protocol`, `server`, `capabilities`, `limits` |
| `open` | `a`, `b`, `mem?` | `session`, `reused`, `mem_bytes`, `state` |
| `summary` | `session` | the state; when ready, counts, sizes, memory, progress |
| `segments` | `session`, `offset?`, `limit?` | a page of differing segments |
| `hunk` | `session`, `segment` or `line_a`, `context?`, `max_lines?`, `max_line_bytes?` | lines of A and B around one segment |
| `moves` | `session`, `offset?`, `limit?` | a page of moved blocks |
| `set_visible` | `session`, `a` | `null` |
| `close` | `session` | `closed` |

### `initialize`

The first request. Until it is answered, every other request gets
`not_initialized`.

```json
{"protocol": 1, "client": {"name": "rift-vscode", "version": "0.3.0"}}
```

Result:

```json
{
  "protocol": 1,
  "server": {"name": "rift", "version": "0.3.0"},
  "capabilities": {
    "methods": ["initialize", "open", "summary", "segments", "hunk", "moves", "set_visible", "close"],
    "notifications": ["progress", "session_state", "segment_ready"]
  },
  "limits": {
    "max_sessions": 4,
    "max_session_mem": 1073741824,
    "max_total_mem": 2139095040,
    "default_mem": 268435456,
    "idle_timeout_secs": null,
    "max_page": 1000,
    "max_lines": 5000,
    "max_line_bytes": 65536,
    "max_context": 100
  }
}
```

A `protocol` the server does not speak gets `invalid_params` with
`data.supported`, the list of versions it does. `idle_timeout_secs` is
`null` when sessions are never dropped for being idle. The client reads
these numbers; it does not assume them.

`rift serve` sets the memory limits from its flags:

| Flag | `limits` field | Default |
|---|---|---|
| `--max-session-mem SIZE` | `max_session_mem` | `1G` |
| `--max-total-mem SIZE` | `max_total_mem` | `2G` |

`max_total_mem` is what sessions may use in total: the flag less the
8 MiB the server keeps for its own buffers, which lie outside it.

`SIZE` is written as for `--mem`. The other numbers are fixed:
`max_sessions` 4, `default_mem` 256 MiB (as `--mem`, at most
`max_session_mem`), `max_page` 1000, `max_lines` 5000, `max_line_bytes`
65536, `max_context` 100, and `idle_timeout_secs` `null`: `rift serve`
drops a session only when the client closes it or a new one needs its
room. `rift-mcp` keeps its own limits (256 MiB per diff, 512 MiB in
all, five minutes idle).

### `open`

Starts a session on two files and answers at once, in state
`scanning`. The scan builds the segment map in two passes over the
disk, which on large files takes seconds to minutes; it reports through
`progress` (phase `scan`) and ends with a `session_state` notification,
`ready` or `failed`. Refinement then goes on in the background and
reports through `progress` (phase `refine`) and `segment_ready`.

```json
{"a": "/data/old.sql", "b": "/data/new.sql", "mem": "256M"}
```

- `a`, `b`: paths of regular files. A symbolic link in the last
  component, a directory, a device, a FIFO or a socket is refused. Files
  are opened read-only.
- `mem`: the session's heap budget, written as for `--mem` (`64M`, `1G`,
  `300K`, a plain number of bytes). Default `limits.default_mem`, at most
  `limits.max_session_mem`.

Result: `{"session": 1, "reused": false, "mem_bytes": 268435456, "state": "scanning"}`.

`open` itself fails only on what it checks before the scan starts:
`invalid_params`, `file` (both paths are opened and checked first),
`too_large` and `busy`. A scan that fails later, for instance with
`budget_exceeded`, fails the session, not the `open`.

The server keeps one session per pair of files. Opening a pair that is
already open, with neither file changed since (device, inode, length and
modification time), returns that session with `reused: true`, its
original `mem_bytes` and its current `state`, `scanning` or `ready`; the
`mem` asked for is ignored. Opening a pair whose session `failed`
replaces it: the old id becomes unknown and a new session scans with
the `mem` asked for, so a client reopens with `suggested_mem` after a
budget refusal. A changed file gets a new session.

When a new session does not fit `max_sessions` or `max_total_mem`, the
server first drops failed sessions, then idle ones, then the least
recently used ready ones that no request is using; a session it drops
answers later requests with `unknown_session`. A scanning session is
never dropped for room. When that is not enough, the answer is `busy`.

#### Session states

```
open ──> scanning ──> ready
            │  └────> failed ──(open of the same pair)──> new session
            └── close ──> gone (unknown_session)
ready, failed ── close ──> gone
```

| Method | `scanning` | `ready` | `failed` |
|---|---|---|---|
| `summary` | `state`, sizes, `mem_bytes`, scan `progress` | everything | `state`, `error` |
| `segments`, `hunk`, `moves`, `set_visible` | `not_ready` | answered | `not_ready` |
| `close` | cancels the scan | closes | closes |

A failed session holds no budget. Its error is sent once in the
`session_state` notification and repeated by `summary` until the
session is closed or replaced, so a client that missed the notification
still finds it.

Closing a scanning session answers `{"closed": true}` without waiting for
the scan. The id is unknown to every later request at once, no further
notification names it, and the scan stops at its next chunk (at most a
few MiB of reading later) and releases the session's memory then.

### `summary`

```json
{"session": 1}
```

Result of a ready session:

```json
{
  "state": "ready",
  "a": {"lines": 1204940, "bytes": 98812733},
  "b": {"lines": 1204941, "bytes": 98812790},
  "identical": false,
  "equal":   {"segments": 812, "a_lines": 1204001, "b_lines": 1204001},
  "differs": {"segments": 806, "a_lines": 912, "b_lines": 913},
  "approx":  {"segments": 1, "a_lines": 27, "b_lines": 27},
  "differing_segments": 807,
  "moves": "found",
  "moved_blocks": 3,
  "mem_bytes": 268435456,
  "mem_reserved_bytes": 201326592,
  "progress": {"phase": "refine", "done": 120, "total": 807}
}
```

While scanning, only what is known:

```json
{
  "state": "scanning",
  "a": {"bytes": 98812733},
  "b": {"bytes": 98812790},
  "mem_bytes": 268435456,
  "progress": {"phase": "scan", "done": 52428800, "total": 395250046}
}
```

After a failed scan, the error the scan failed with, as a JSON-RPC
error object:

```json
{"state": "failed", "error": {"code": -32003, "message": "...", "data": {"mem_bytes": 1048576, "half": "skeleton", "requested_bytes": 2097152, "available_bytes": 0, "suggested_mem_bytes": 5242880, "suggested_mem": "5120K"}}}
```

- `approx` counts the refined segments that came out approximate, out of
  the `progress.done` refined so far, and `differs` the other differing
  segments: not refined yet, or refined exactly. Together they are
  `differing_segments`. Both change while refinement runs, and `approx`
  is final once `done` reaches `total`. In Rift 0.3.0, `approx` counted
  segments by their status in the map, which never holds `approx`, so it
  was always 0 and `differs` counted every differing segment.
- `moves`: `found` (the search ran; `moved_blocks` may be 0), `off` (the
  server was started without move detection), or `skipped` (the budget
  did not allow the search, so 0 blocks says nothing about the files).
- `progress`: in phase `refine`, segments refined so far, of those that
  need it.

### `segments`

A page of the differing segments, in file order. Equal segments are not
listed; `summary` counts them.

```json
{"session": 1, "offset": 0, "limit": 100}
```

`offset` (default 0) counts differing segments, not map indices.
`limit`: default 100, at most `limits.max_page`.

Result:

```json
{
  "total": 807,
  "offset": 0,
  "segments": [
    {"index": 3, "a": {"start": 1204880, "count": 61}, "b": {"start": 1204877, "count": 65}, "status": "differs"},
    {"index": 5, "a": {"start": 1204952, "count": 27}, "b": {"start": 1204953, "count": 27}, "status": "approx"}
  ]
}
```

`index` is the segment's position in the full map, the number `hunk`
takes. `status` is the map status: `differs` or `approx`. A segment's
range can take in a few equal lines next to the change.

### `hunk`

The lines of one segment on both sides, with context, for a client to
show or diff itself. It never waits for refinement: the lines come
straight from the files.

```json
{"session": 1, "segment": 3, "context": 3, "max_lines": 1000, "max_line_bytes": 4096}
```

- Exactly one of `segment` (a map index) or `line_a` (a line of A). With
  `line_a` the server picks the segment whose A range holds that line.
- `context`: lines before and after the segment, default 3, at most
  `limits.max_context`. Context stops at the start and end of each file.
- `max_lines`: lines per side, default 1000, at most `limits.max_lines`.
- `max_line_bytes`: bytes of one line's text, default 4096, at most
  `limits.max_line_bytes`.

A differing segment is shown from its first line. An equal segment
reached through `line_a` is shown from that line, since its lines match
one for one; through `segment`, from its first line.

Result:

```json
{
  "segment": 3,
  "status": "differs",
  "pending": false,
  "range": {"a": {"start": 1204880, "count": 61}, "b": {"start": 1204877, "count": 65}},
  "a": {
    "start": 1204877,
    "total": 67,
    "lines": [
      {"text": "INSERT INTO t VALUES (1, 'x');", "bytes": 30, "eol": "lf"},
      {"text": "caf�", "bytes": 4, "eol": "lf", "lossy": true},
      {"text": "aaaaaaaa", "bytes": 1048576, "eol": "lf", "truncated": true},
      {"text": "INSERT INTO t VALUES (2, 'y');", "bytes": 30, "eol": "lf", "moved": true}
    ]
  },
  "b": {"start": 1204874, "total": 71, "lines": []},
  "truncated": true
}
```

- `status`: `equal`, `differs` or `approx`. `approx` when the map says
  so or when the refined edit script was cut short.
- `pending`: refinement of this segment has not finished, so `status`
  may still change from `differs` to `approx`. Asking for a hunk queues
  its segment for refinement; `segment_ready` follows.
- `range`: the segment itself, without context.
- `a`, `b`: `start` is the line number of the first entry in `lines`;
  the entries are consecutive lines. `total` is how many lines that side
  has with context, before `max_lines` and the byte limit.
- `truncated`: a side shows fewer than `total` lines.

A line:

| Field | Meaning |
|---|---|
| `text` | the line without its terminator, at most `max_line_bytes` bytes of it, decoded as UTF-8 with each invalid sequence replaced by U+FFFD |
| `bytes` | the full length of the line in the file, terminator excluded |
| `eol` | `lf`, `crlf`, or `none` for a last line without a terminator |
| `truncated` | `text` holds only the first `max_line_bytes` bytes; a UTF-8 sequence cut by that limit is dropped, not replaced |
| `lossy` | the shown bytes are not valid UTF-8, so `text` does not give them back exactly |
| `moved` | the line lies in a moved block on its side |

### `moves`

A page of the moved blocks, ordered by their line in A.

```json
{"session": 1, "offset": 0, "limit": 100}
```

Result:

```json
{
  "total": 3,
  "offset": 0,
  "moves": "found",
  "blocks": [{"a": {"start": 10, "count": 5}, "b": {"start": 900, "count": 5}}]
}
```

A moved block is two ranges of the same length holding the same bytes.
It annotates the diff and never changes the edit script.

### `set_visible`

The A lines the client shows, so the server refines those segments
first. Inserted lines count with the A line they follow.

```json
{"session": 1, "a": {"start": 1204870, "count": 80}}
```

Result: `null`. Each call replaces the previous range.

### `close`

```json
{"session": 1}
```

Result: `{"closed": true}`, or `false` when no such session was open,
so closing twice is not an error. Requests already running on the
session finish; its memory is released when the last one does.

## Notifications from the server

| Method | Params |
|---|---|
| `progress` | `session`, `phase` (`scan` or `refine`), `done`, `total` |
| `session_state` | `session`, `state` (`ready` or `failed`), `error` when failed |
| `segment_ready` | `session`, `index`, `status` (`differs` or `approx`) |

- `progress`, phase `scan`: bytes the scan has read, of `total`, which
  is twice the size of both files (two passes). `done` only grows and
  reaches `total` when the scan succeeds; a failed scan stops short.
  Phase `refine`: segments refined so far, of `total`. Sent while the work advances, at most 20 per
  second per session, and once more when `done` reaches `total`.
- `session_state`: the scan ended. Sent once per session, after the last
  scan `progress`; `error` is the error object described under
  `summary`. Not sent for a session closed while scanning.
- `segment_ready`: a segment's refinement finished. Sent only for
  segments in the last `set_visible` range and segments asked for with
  `hunk`, so a background pass over a million segments does not flood
  the client. Counted in the same 20 per second per session; segments
  that finish in the same interval are sent together then.

## Cancellation

The client cancels a request with the notification
`$/cancelRequest {"id": <request id>}`, as in LSP. The request is
answered with `cancelled`; a cancelled request gets no other answer.
Cancelling a request that has already been answered does nothing. A
scan is not a request: `close` stops it.

## Errors

JSON-RPC error objects: `code`, `message`, and `data` where the table
says. `message` is for people and may change; clients branch on `code`
and read `data`.

Rift's own codes are in -32001 to -32099, the range JSON-RPC leaves to
the server. `not_initialized` and `cancelled` take LSP's codes, which
LSP client libraries already know.

| Code | Name | When | `data` | From |
|---|---|---|---|---|
| -32700 | `parse_error` | the body is not JSON | none | the server's reader |
| -32600 | `invalid_request` | not a JSON-RPC request, or a batch | none | the server's reader |
| -32601 | `method_not_found` | unknown method | none | the server's reader |
| -32602 | `invalid_params` | params do not match the schema, `mem` does not parse, a `segment` or `line_a` past the end, a `protocol` the server does not speak | `param`; `value` (as sent) and `limit` for a number out of range; `supported` for `protocol` | `rift_session::Error::{SegmentOutOfRange, LineOutOfRange}`; `rift_core::ParseBytesError` |
| -32603 | `internal_error` | a bug: a refinement panicked, an invalid edit script, a line past the end of a file the server indexed | none | `rift_core::Error::{InvalidScript, LineOutOfRange, LineTooLong, Keyed}`, a panic |
| -32002 | `not_initialized` | a request before `initialize` | none | the server |
| -32800 | `cancelled` | the client cancelled the request, or standard input closed | none | the server |
| -32001 | `file` | a path cannot be opened for the diff | `side`, `path`, `reason` | `rift_session::Error::File` (`std::io::Error` from `lstat` or `open`) |
| -32003 | `budget_exceeded` | the scan (in a failed session's `error`), or a refinement run for this request, does not fit `mem` | `mem_bytes`, `half`, `requested_bytes`, `available_bytes`, `suggested_mem_bytes?`, `suggested_mem?` | `rift_core::Error::Budget(BudgetExceeded)` |
| -32004 | `too_large` | `mem` is over `limits.max_session_mem` | `mem_bytes`, `max_mem_bytes` | `rift_session::Error::MemOverLimit` |
| -32005 | `busy` | no room for another session, or the session's response pool is in use | `what`: `sessions` or `responses` | `rift_session::Error::{Busy, ResponsesBusy}` |
| -32006 | `unknown_session` | the session was closed, dropped for room or idleness, or never existed | `session` | `rift_session::Error::UnknownSession` |
| -32007 | `io` | a file failed to read after it was opened, for instance it was truncated | none | `rift_core::Error::Io` |
| -32008 | `not_ready` | `segments`, `hunk`, `moves` or `set_visible` on a session that is `scanning` or `failed` | `session`, `state`; scan `progress` while scanning; `error` when failed | `rift_session::Error::NotReady` |

`file` reasons: `not_found`, `permission_denied`, `not_regular` (a
directory, device, FIFO or socket), `symlink`, `changed` (the path was
replaced while it was being opened), `io` (anything else).

`budget_exceeded` fields:

- `mem_bytes`: the budget the session runs with.
- `half`: `skeleton` or `refine`, the half that refused.
- `requested_bytes`, `available_bytes`: what was asked and what the half
  had free.
- `suggested_mem_bytes`, `suggested_mem`: a budget to reopen with, in
  bytes and in the form `open` takes (`"10240K"`). The same value
  `rift` prints as `rerun with --mem 10240K`, from the same code. It is
  twice what the refusing half held plus what it asked for, rounded up
  by less than 1/64. It is not a lower bound and not a promise: a later
  stage may need more, and the client compares it with
  `limits.max_session_mem` before offering it. Left out when the
  refusal does not exceed what `mem` already gives that half.

A refinement that runs out of budget does not fail the session: the
server falls back to an approximate script where it can, and only a
`hunk` whose segment cannot be refined at all reports
`budget_exceeded`.

## Bounds

Every response has a size bound that does not depend on the files. The
large ones are built under memory the session's budget pays for.

| Response | Bound |
|---|---|
| `initialize`, `open`, `summary`, `set_visible`, `close`, notifications | fixed fields, under 2 KiB, plus one error object in a failed session's `summary` and `session_state` |
| errors | under 1 KiB plus, for `file`, the path twice |
| `segments` | `limit` entries, at most `limits.max_page`; each under 200 bytes |
| `moves` | `limit` blocks, at most `limits.max_page`; each under 100 bytes |
| `hunk` | per side, at most `max_lines` lines of at most `max_line_bytes` bytes each before decoding; and the text of both sides together within the session's response pool |

**Response pool.** When a session is opened, `rift-session` reserves a
response pool from the Refine half of that session's budget: 1/16 of the
half, at most 8 MiB. A `hunk` builds its lines inside that pool and
stops, with `truncated: true`, when the next line would not fit. So
decoded text is counted where it is held: a lossy byte that becomes a
three-byte U+FFFD costs three. Two `hunk` requests that together need
more than the pool get `busy` (`what: responses`) for the later one.
The pool is part of `mem`, so a larger `mem` allows larger hunks. On a
`mem` so small that one line of `max_line_bytes` per side would not fit
the empty pool, lines are cut shorter instead.

Pages from `segments` and `moves` borrow from the segment map and the
block list, which the session holds anyway, and need no pool.

The server writes each response straight to standard output through a
fixed buffer rather than building it as one string. JSON escaping can
make a line up to six times its text (`\u0000`); that is in the stream,
not on the heap.

## Line text and non-UTF-8 bytes

Rift compares bytes. The protocol carries text: each line is decoded as
UTF-8, invalid sequences become U+FFFD, and the line says `lossy`.

Why not exact bytes: every client shows lines as text. VS Code's virtual
documents are strings, the TreeView shows strings, an MCP client reads
text. None of them applies a patch from an excerpt, and an excerpt can
not be exact anyway once a long line is cut at `max_line_bytes`. Base64
would cost a third more on every line and a decoding step in every
client for no reader that needs it. Exactness stays where it matters,
in `rift`'s own output, which is a byte-exact edit script.

The flag keeps the loss honest: two lines that differ only in invalid
bytes can decode to the same text, and `lossy` tells the client why
VS Code shows no difference. The same goes for the terminator, which is
why `eol` is carried: `a\n` and `a\r\n` are different lines to Rift and
the same text to an editor.

If a client needs exact bytes later, a new protocol version can add an
encoding parameter; version 1 does not have one.
