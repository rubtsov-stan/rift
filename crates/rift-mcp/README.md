# rift-mcp

An MCP server on standard input and output that answers questions about the diff of two large files, so a client reads a few kilobytes instead of the files: a summary, the lines of one change, the next difference after a line, and for CSV, NDJSON and `pg_dump` records, the keys added, removed or changed.

```
cargo install rift-mcp
```

The [MCP guide](https://rubtsov-stan.github.io/rift/guide/mcp/) has the tools, the limits on every answer and the setup for each client. It is listed in the MCP Registry as `io.github.rubtsov-stan/rift-mcp`.

Licensed under MIT or Apache-2.0, at your option.
