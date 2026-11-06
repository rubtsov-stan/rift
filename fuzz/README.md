# Fuzzing

cargo-fuzz targets for rift, and `run-linux.sh`, which builds and runs all of them on a Linux machine you care about and packs the results into one archive. This crate is its own workspace: `cargo` in the repository root never builds it.

## Targets

| Target | What it drives | Property |
|---|---|---|
| `scan` | `scan::scan` with chunk size, threads and caps from the input | line count, checkpoints and samples match a plain split; samples equal a one-chunk scan's; an index-backed reader returns every line whole, as a prefix and in pieces |
| `keyed_csv`, `keyed_ndjson`, `keyed_copy` | `keyed::compare` with a CSV, NDJSON or `COPY` key | a small and a large budget report the same changes, or one fails for a budget reason only; a file against itself reports nothing; peak heap within the small budget |
| `pipeline` | `Diff::new` and `write_unified`, as `rift A B --mem M` runs them | the unified diff applied to A gives B; the verdict matches A == B; peak heap, from the counting allocator, within `--mem` |
| `unified` | `unified::write_diff` on scripts built from the input | the output applied to A gives B; with one edit moved, the writer accepts the script exactly when `script::validate` does |
| `tui_render` | `rift_tui::App::draw` on a test backend, after keys from the input | no cell holds a control, bidirectional, zero-width or other default-ignorable character |
| `mcp_request` | JSON-RPC parsing, then a `rift-mcp` tool call through an in-process client | no panic or hang; answers keep the documented size bounds |

The checks live in `src/`, one module per target family; `fuzz_targets/` holds one line per target. Each module documents its input layout. The patch applier in `src/lib.rs` is the oracle for both diff targets.

Seeds are in `corpus/<target>/`: hand-written edge cases, and pairs from `rift-gen --size 2K` with seed 7.

## Running on Linux

```sh
fuzz/run-linux.sh --dry-run          # print the plan, change nothing
fuzz/run-linux.sh                    # 10 s to cancel, then 30 min per target
fuzz/run-linux.sh --minutes 60 --yes # longer, no wait
```

Options: `--minutes N` per target (default 30; the plan prints the total), `--ref REF` (default `HEAD`), `--repo URL` (default the checkout the script is in; only committed changes are part of the run, and a URL given here appears in the report).

Needs: Linux, a non-root user, rustup with a nightly toolchain (`rustup toolchain install nightly --profile minimal`; the script stops with that line if nightly is missing), `git`, `awk`, `tar`, network access for crates, and a systemd user session or `prlimit`. `cargo-fuzz` is used from `PATH`, or built into the scratch directory when missing.

The result is `rift-fuzz-linux-<cpu>-<date>.tar.gz` in the current directory, labelled by CPU model and date, never by hostname or user:

- `report.md`: host, plan, and per target the run time, executions, exec/s, libFuzzer's coverage and features, corpus files and bytes before and after, and crash, timeout and OOM counts;
- `results.json`: the same, `schema_version` 1;
- `artifacts/<target>/`: every input that crashed (`crash-*`), timed out (`timeout-*`) or ran out of memory (`oom-*`);
- `logs/<target>.log`: libFuzzer's output, with the stack trace of each crash (first 8 MiB).

### What it does to the machine

- Writes one directory in tmpfs, `/dev/shm/rift-fuzz-<pid>` (or under `$XDG_RUNTIME_DIR`), after checking with `stat -f` that it is tmpfs with 3 GiB free. It refuses to run otherwise.
- The clone, `CARGO_HOME`, `CARGO_TARGET_DIR`, the corpora, the targets' temporary files (`TMPDIR`) and the logs live there. After the build only the eight binaries stay; the target directory and crate cache are deleted.
- A trap on exit, error, Ctrl-C and termination stops the running fuzzer and removes the directory.
- The only disk write is the archive. `systemd-run` leaves a few lines in the journal.
- No root, no `sudo`, no kernel settings. It never installs a toolchain (`RUSTUP_AUTO_INSTALL=0`).
- Each target runs as `systemd-run --user --scope -p MemoryMax=<cap> -p MemorySwapMax=0 nice -n 10 ionice -c3 <target> -fork=<k> ...`, with AddressSanitizer. Without a systemd user session it uses `prlimit --as` per process instead, and builds without AddressSanitizer, which needs more address space than such a limit allows; the report says so. With neither, it does not run.
- Sizing, all within 60% of `MemAvailable`: the build needs 3 GiB of tmpfs, 3 GiB for four compiler jobs and a 2 GiB margin, so the script stops below 13.3 GiB of `MemAvailable`. Fuzzing runs one target at a time with `k` worker processes, each under libFuzzer's 2 GiB RSS limit and counted at 2.5 GiB, plus 0.5 GiB for the parent, 1 GiB of tmpfs and the margin. That gives `k = 2` at 16 GiB of `MemAvailable` and `k = 6` at 32 GiB, never more than one less than the CPU count.
- libFuzzer runs in fork mode with `-ignore_crashes=1 -ignore_timeouts=1 -ignore_ooms=1`, so a crash does not end a target's run; `-timeout=10` per input; leak detection is off, since growing memory hits the RSS limit and lazily built statics would read as leaks.

## Turning a crash into a regression test

1. Unpack the archive and read the crash's stack trace in `logs/<target>.log`.
2. Copy the input to `fuzz/regressions/<target>/<short-name>` and replay every seed and saved input through the checks, without libFuzzer, on any OS:

   ```sh
   cargo test --manifest-path fuzz/Cargo.toml -- --test-threads=1
   ```

   One thread, because the heap checks read process-wide counters. On macOS, if the libFuzzer build cannot find C++ headers, set `CXXFLAGS="-isystem $(xcrun --show-sdk-path)/usr/include/c++/v1"`.
3. On Linux, `cargo +nightly fuzz run <target> fuzz/regressions/<target>/<short-name>` reproduces it under the fuzzer's build.
4. Write a focused test in the crate that owns the bug, fix it, and keep the input in `fuzz/regressions/` so the replay test covers it too.

## Building here

```sh
cargo +nightly fuzz build    # all targets, AddressSanitizer
```

The test knobs at the top of `run-linux.sh` (`RIFT_FUZZ_TESTING`, `RIFT_FUZZ_MEMINFO`, `RIFT_FUZZ_SHM`, `RIFT_FUZZ_LIMITER`, `RIFT_FUZZ_BINS`) let you check the plan, the report and the cleanup on another OS with stand-in executables. They turn safety checks off; a real run must not set them.
