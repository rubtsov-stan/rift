# rift-diff-session

Live diffs of file pairs for [Rift](https://rubtsov-stan.github.io/rift/), shared by `rift serve` and `rift-mcp`.

It opens a pair of files by path (regular files only, no symbolic links), scans them on a thread of its own, keeps a bounded number of sessions within a memory limit, and answers the questions of the [protocol](https://github.com/rubtsov-stan/rift/blob/main/docs/protocol.md): summary, differing ranges, hunks and excerpts. A file that changes after it was opened fails the request that notices it. In code the crate is `rift_session`.

It is published so that [`rift-diff`](https://crates.io/crates/rift-diff) and [`rift-mcp`](https://crates.io/crates/rift-mcp) can be installed with `cargo install`; its API follows their needs.

Licensed under MIT or Apache-2.0, at your option.
