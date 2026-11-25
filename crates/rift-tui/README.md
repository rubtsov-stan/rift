# rift-diff-tui

The terminal interface of [Rift](https://rubtsov-stan.github.io/rift/), built on ratatui: a map of the differences and a view that jumps between them while the diff is still being refined.

It sees the diff only through `rift_core::DiffView` and line text through two `rift_core::LineAccess` handles, and keeps nothing per segment, so its memory does not grow with the files. In code the crate is `rift_tui`.

It is published so that [`rift-diff`](https://crates.io/crates/rift-diff) can be installed with `cargo install` (`rift -i`).

Licensed under MIT or Apache-2.0, at your option.
