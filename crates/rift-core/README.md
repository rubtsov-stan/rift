# rift-diff-core

The diff engine of [Rift](https://rubtsov-stan.github.io/rift/): a line diff for very large files that keeps its heap under a byte budget you set.

The crate reads input only through its `Source` trait and knows nothing about the terminal or the file system. Every data structure is sized from a `Budget`. The output is an edit script that turns the first file into the second exactly; a region it could not refine within the budget is marked approximate. In code the crate is `rift_core`.

Most people want the command line tool, [`rift-diff`](https://crates.io/crates/rift-diff). The invariants and the meaning of each type are in [DESIGN.md](https://github.com/rubtsov-stan/rift/blob/main/DESIGN.md).

Licensed under MIT or Apache-2.0, at your option.
