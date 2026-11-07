# Fuzzing

cargo-fuzz targets for rift, and `run-linux.sh`, which builds and runs all of them on a Linux machine you care about and packs the results into one archive. This crate is its own workspace: `cargo` in the repository root never builds it.

## Targets

| Target | What it drives | Property |
|---|---|---|
| `scan` | `scan::scan` with chunk size, threads and caps from the input | line count, checkpoints and samples match a plain split; samples equal a one-chunk scan's; an index-backed reader returns every line whole, as a prefix and in pieces |
| `keyed_csv`, `keyed_ndjson`, `keyed_copy` | `keyed::compare` with a CSV, NDJSON or `COPY` key | a small and a large budget report the same changes, or one fails for a budget reason only; a file against itself reports nothing; peak heap within the small budget |
| `keyed_reader` | the record reader behind key mode, for every key kind, with window and chunk counts from the input | 1, 4 and 16 chunks read the same records; a rewound reader reads them again; keys hash as `key_hash` says and match an independent parse of simple CSV and NDJSON records; a filter on half the hash range keeps that half |
| `pipeline` | `Diff::new` and `write_unified`, as `rift A B --mem M` runs them | the unified diff applied to A gives B; the verdict matches A == B; peak heap, from the counting allocator, within `--mem` |
| `unified` | `unified::write_diff` on scripts built from the input | the output applied to A gives B; with one edit moved, the writer accepts the script exactly when `script::validate` does |
| `tui_render` | `rift_tui::App::draw` on a test backend, after keys from the input | no cell holds a control, bidirectional, zero-width or other default-ignorable character |
| `sanitize` | the interface's line sanitizer alone | no hidden character in the output; one column per character within the budget; a smaller budget gives a prefix; plain text passes unchanged |
| `mcp_request` | JSON-RPC parsing, then a `rift-mcp` tool call through an in-process client | no panic or hang; answers keep the documented size bounds |
| `mcp_tools` | session lookup, the three tools and `render::hunk_text`, without the protocol | as `mcp_request`, and a rendered hunk has one text line per shown line within the line limit |

The checks live in `src/`, one module per target family; `fuzz_targets/` holds one line per target. The direct targets (`keyed_reader`, `sanitize`, `mcp_tools`) reach internals that rift-core, rift-tui and rift-mcp expose only under `--cfg fuzzing`, which cargo-fuzz sets; a normal build has no such items. Each module documents its input layout. The patch applier in `src/lib.rs` is the oracle for both diff targets.

Seeds are in `corpus/<target>/`: hand-written edge cases, and pairs from `rift-gen --size 2K` with seed 7.

## Two ways to run

- On a Linux machine you own: `run-linux.sh`, below. Several targets at once, each pinned to its own cores and memory cap, results in one archive.
- In CI: the `Fuzz` workflow, [at the end](#in-ci).

## Running on Linux

```sh
fuzz/run-linux.sh --dry-run                # print the plan, change nothing
fuzz/run-linux.sh                          # 10 s to cancel, then 30 min per target
fuzz/run-linux.sh --time 600 --jobs 4 --yes # 10 min per target, 4 at once, no wait
```

Options: `--time S` seconds per target (default 1800; `--minutes N` still works and means `--time $((N * 60))`), `--jobs N` targets at once (default: physical cores minus 2, at least 1), `--ref REF` (default `HEAD`), `--repo URL` (default the checkout the script is in; only committed changes are part of the run, and a URL given here appears in the report).

The plan prints the number of jobs, the cores of each slot, the memory cap per target and the total time (targets / jobs, rounded up, times `--time`). Each slot gets whole physical cores, hardware threads of one core together, and the target in it runs under `taskset -c <those cores>`, so libFuzzer's `-fork` workers stay on them.

Needs: Linux, `taskset` (util-linux), a non-root user, rustup with a nightly toolchain (`rustup toolchain install nightly --profile minimal`; the script stops with that line if nightly is missing), `git`, `awk`, `tar`, network access for crates, and a systemd user session or `prlimit`. `cargo-fuzz` is used from `PATH`, or built into the scratch directory when missing.

The result is `rift-fuzz-linux-<cpu>-<date>.tar.gz` in the current directory, labelled by CPU model and date, never by hostname or user:

- `report.md`: host, plan, and per target the run time, executions, exec/s, libFuzzer's coverage and features, corpus files and bytes before and after, crash, timeout and OOM counts, and the peak memory of the target's scope;
- `results.json`: the same, `schema_version` 3 (3 added `memory_peak_bytes` per target: the scope's `MemoryPeak` in bytes, `null` when unknown);
- `artifacts/<target>/`: every input that crashed (`crash-*`), timed out (`timeout-*`) or ran out of memory (`oom-*`);
- `logs/<target>.log`: libFuzzer's output, with the stack trace of each crash (first 8 MiB).

### What it does to the machine

- Writes one directory in tmpfs, `/dev/shm/rift-fuzz-<pid>` (or under `$XDG_RUNTIME_DIR`), after checking with `stat -f` that it is tmpfs with 3 GiB free. It refuses to run otherwise.
- The clone, `CARGO_HOME`, `CARGO_TARGET_DIR`, the corpora, the targets' temporary files (`TMPDIR`) and the logs live there. After the build only the eleven binaries stay; the target directory and crate cache are deleted.
- A trap on exit, error, Ctrl-C and termination stops the running fuzzer and removes the directory.
- The only disk write is the archive. `systemd-run` leaves a few lines in the journal.
- No root, no `sudo`, no kernel settings. It never installs a toolchain (`RUSTUP_AUTO_INSTALL=0`).
- Each target runs as `systemd-run --user --scope -p MemoryMax=<cap> -p MemorySwapMax=0 nice -n 10 ionice -c3 taskset -c <cores> <target> -fork=<k> ...`, with AddressSanitizer. Without a systemd user session it uses `prlimit --as` per process instead, and builds without AddressSanitizer, which needs more address space than such a limit allows; the report says so. With neither, it does not run.
- Sizing, all within 60% of `MemAvailable`: the build needs 3 GiB of tmpfs, 3 GiB for four compiler jobs and a 2 GiB margin, so the script stops below 13.3 GiB of `MemAvailable`. Fuzzing runs `N` targets at once. A fork worker is counted at 1.5 GiB (libFuzzer's 1 GiB RSS limit plus slack), the parent at 0.5 GiB, so a slot with one worker is 2 GiB; 1 GiB of tmpfs and the 2 GiB margin are shared. When `N` targets with one worker each do not fit, `N` is lowered and the plan says so: 2 at 13 GiB of `MemAvailable`, 3 at 16 GiB, the default 4 at 32 GiB. Workers per target are then as many as the slot's cores and the remaining memory allow (one at 13 and 16 GiB, two at 32 GiB); the cap is the same for every target. The 1 GiB limit (`rss_mb` in the script, used for the sizing, the scope's `MemoryMax` and `-rss_limit_mb`) is a cap, not a measurement. Each target's `MemoryPeak` is sampled once a second while it runs and lands in `results.json` and the report; tune `rss_mb` from those figures. They are `unknown` under `prlimit` or when systemd does not report the property (it needs a recent systemd and cgroup v2).
- libFuzzer runs in fork mode with `-ignore_crashes=1 -ignore_timeouts=1 -ignore_ooms=1`, so a crash does not end a target's run; `-timeout=10` per input; leak detection is off, since growing memory hits the RSS limit and lazily built statics would read as leaks.

## Turning a crash into a regression test

1. Unpack the archive and read the crash's stack trace in `logs/<target>.log`.
2. Copy the input to `fuzz/regressions/<target>/<short-name>` and replay every seed and saved input through the checks, without libFuzzer, on any OS:

   ```sh
   RUSTFLAGS="--cfg fuzzing" cargo test --manifest-path fuzz/Cargo.toml -- --test-threads=1
   ```

   The flag exposes the internals the direct targets use. One thread, because the heap checks read process-wide counters. On macOS, if the libFuzzer build cannot find C++ headers, set `CXXFLAGS="-isystem $(xcrun --show-sdk-path)/usr/include/c++/v1"`.
3. On Linux, `cargo +nightly fuzz run <target> fuzz/regressions/<target>/<short-name>` reproduces it under the fuzzer's build.
4. Write a focused test in the crate that owns the bug, fix it, and keep the input in `fuzz/regressions/` so the replay test covers it too.

## Building here

```sh
cargo +nightly fuzz build    # all targets, AddressSanitizer
```

The test knobs at the top of `run-linux.sh` (`RIFT_FUZZ_TESTING`, `RIFT_FUZZ_MEMINFO`, `RIFT_FUZZ_SHM`, `RIFT_FUZZ_LIMITER`, `RIFT_FUZZ_BINS`) (plus `RIFT_FUZZ_ALLOWED`, `RIFT_FUZZ_TOPO` and `RIFT_FUZZ_TASKSET_LOG` for the core layout and the pinning arguments) let you check the plan, the report and the cleanup on another OS with stand-in executables. They turn safety checks off; a real run must not set them.

## In CI

`.github/workflows/fuzz.yml` runs every night and on demand:

```sh
gh workflow run fuzz.yml
gh run list --workflow fuzz.yml
```

Each target gets its own runner and 10 minutes of fuzzing (`-max_total_time=600`). The corpus of each target is restored from and saved to the Actions cache, starting from the seeds in `corpus/<target>/`, so it grows from run to run. GitHub evicts caches that go unused for 7 days.

A crash, timeout or out-of-memory input fails that job, and only that job: the other targets keep running. The run shows red in the Actions tab, and the failing job uploads an artifact `fuzz-<target>` with the input (`crash-*`, `timeout-*` or `oom-*`) and the libFuzzer log. Download it with `gh run download <run-id> -n fuzz-<target>` and continue as in "Turning a crash into a regression test".
