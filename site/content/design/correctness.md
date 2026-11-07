+++
title = "Correctness: what Rift guarantees and how it is tested"
description = "What Rift guarantees, from a correct edit script to a heap under --mem, and the property tests, fuzz targets, Miri runs and CI checks behind each one."
weight = 7
[extra]
nav = "Correctness"
h1 = "Correctness"
lead = "A diff tool that is fast and wrong is worse than none. Each guarantee below has a check in the repository that fails when the guarantee breaks."
summary = "What is guaranteed and how it is checked: property tests, byte-identical tests, 11 fuzz targets, Miri and CI memory checks."
+++

[DESIGN.md](https://github.com/rubtsov-stan/rift/blob/main/DESIGN.md) lists the guarantees under "Guarantees" and what is left out under "Non-guarantees".

## What is guaranteed

1. **The output is a correct edit script.** Applied to A, it yields exactly B, approximate regions included.
2. **Equal means compared.** A segment or a line is reported equal only after Rift compared its bytes. A matching 64-bit hash is never enough.
3. **Approximate regions are marked** in the unified diff (`approx` after the hunk header), in `--summary` and `--stats`, and in the terminal view.
4. **Peak heap stays under `--mem`.**
5. **Exit codes follow `diff`:** 0 for identical files, 1 for different ones, 2 for an error.

And what is not: the diff is not minimal, it can differ between two values of `--mem`, and moved blocks are listed only when one of their lines was sampled.

## Which check covers which guarantee

<figure class="diagram">
<svg viewBox="0 0 720 260" role="img" aria-labelledby="cor-t cor-d">
<title id="cor-t">Guarantees and the checks that cover them</title>
<desc id="cor-d">A grid with five guarantees as rows and five kinds of check as columns: property tests, integration tests, fuzzing, the CI memory job and Miri. A correct edit script is checked by property tests, integration tests and fuzzing. Equal means compared is checked by property tests and integration tests. Approximate regions marked is checked by integration tests. Peak heap under --mem is checked by integration tests, fuzzing and the CI memory job. The unsafe code is checked by Miri.</desc>
<text x="300" y="24" style="font-size:12px">property</text><text x="380" y="24" style="font-size:12px">integration</text><text x="480" y="24" style="font-size:12px">fuzzing</text><text x="560" y="24" style="font-size:12px">CI memory</text><text x="650" y="24" style="font-size:12px">Miri</text>
<path class="link" d="M10 36 H710 M10 80 H710 M10 124 H710 M10 168 H710 M10 212 H710 M10 256 H710"/>
<text x="10" y="63">edit script yields B</text><text x="10" y="107">equal means compared</text><text x="10" y="151">approx is marked</text><text x="10" y="195">peak heap under --mem</text><text x="10" y="239">unsafe code is sound</text>
<circle class="eq" cx="326" cy="58" r="8"/><circle class="eq" cx="418" cy="58" r="8"/><circle class="eq" cx="508" cy="58" r="8"/>
<circle class="eq" cx="326" cy="102" r="8"/><circle class="eq" cx="418" cy="102" r="8"/>
<circle class="eq" cx="418" cy="146" r="8"/>
<circle class="eq" cx="418" cy="190" r="8"/><circle class="eq" cx="508" cy="190" r="8"/><circle class="eq" cx="594" cy="190" r="8"/>
<circle class="eq" cx="666" cy="234" r="8"/>
</svg>
<figcaption>A filled dot means a check in that column fails when the guarantee in that row breaks. Exit codes have an integration test of their own, <code>exit_codes_follow_diff</code>.</figcaption>
</figure>

## Property tests

`proptest` drives the refiner with 10,000 cases per property. `script_applied_to_a_yields_b` and `script_for_mutated_input_yields_b` replay the script against A and require B, the second with random edits on a base text and a cost cutoff from 0 to 3, so the approximate path runs too. `myers_and_dp_match_imara_minimal` turns patience off and requires an edit count equal to the longest common subsequence and no larger than the `imara-diff` crate's. `colliding_hashes_on_random_input` hashes each line to one of three values on purpose, so almost every hash match is false, and the script must still be exact: the byte check after Myers is what saves it.

Smaller suites cover the rest of the pipeline. `random_pairs_replay_under_any_budget` runs whole diffs at 16, 24 and 64 MiB; `the_large_segment_path_replays_random_pairs` sends random pairs through resampling with job limits from 2 to 64 KiB; `patch_applies_our_diff_to_a_and_gets_b` writes the unified diff and runs `patch` and `gpatch`, when present, on it; `random_scripts_validate_and_apply_to_b` checks the validator against the applier.

Sources: `crates/rift-core/src/refine/tests.rs`, `crates/rift-core/src/diff/tests.rs`, `crates/rift-core/src/unified/tests.rs`, `crates/rift-core/src/script/mod.rs`.

## Integration tests on real files

The `rift` binary is tested as you run it. `generated_pairs_patch_from_a_to_b` diffs generated SQL, CSV, NDJSON and text pairs, with long lines, CRLF endings, a missing final newline, duplicated and low-entropy runs and moves, at `--mem 16M` and `256M`, and applies each diff with `patch`. `pathological_inputs` covers empty files, files of blank lines and a missing final newline. `a_single_1gib_line`, run by hand in release mode, diffs two files of one 1 GiB line each, checks the heap stays under 256 MiB, and patches A into B with GNU patch.

`differential_against_git_patience` compares Rift with `git diff --no-index --patience` on 40 generated 64 KiB pairs. Both must patch A into B, and Rift may change at most 10% more lines than git. In a run on 2026-11-07 on an Apple M1 (`cargo test --release -p rift-diff --test full differential_against_git_patience -- --nocapture`), the two changed the same number of lines: 1,241 each.

Equality is tested where hashes lie. `a_false_anchor_is_demoted_and_never_equal` uses a source whose bytes change between the scan and the comparison, which looks to the skeleton like a hash collision, and requires the line to land in a differing segment. `approximate_regions_are_marked_and_still_patch` requires the `approx` mark and a working patch on a region with no unique line.

Some tests compare whole outputs byte for byte. `no_moves_leaves_the_unified_diff_unchanged` requires the same unified diff with and without `--no-moves`; in key mode, `reordered_rows_are_identical` requires that a file whose rows were only shuffled reports no change.

Sources: `crates/rift-cli/tests/full.rs`, `crates/rift-cli/tests/moves.rs`, `crates/rift-cli/tests/keyed.rs`; `crates/rift-core/tests/skeleton.rs`.

## Fuzzing: 11 targets, every night

The fuzz crate under `fuzz/` has 11 cargo-fuzz targets. Each one checks a property beyond "does not crash":

| Target | Property |
|---|---|
| `scan` | line count, checkpoints and samples match a plain split, for any chunk size and thread count |
| `keyed_csv`, `keyed_ndjson`, `keyed_copy` | a small and a large budget report the same changes; a file against itself reports nothing; peak heap within the small budget |
| `keyed_reader` | 1, 4 and 16 chunks read the same records |
| `pipeline` | the unified diff applied to A gives B; the verdict matches A == B; peak heap within `--mem` |
| `unified` | the writer's output applied to A gives B |
| `tui_render` | no control, bidirectional or zero-width character reaches the terminal |
| `sanitize` | the line sanitizer hides nothing and keeps to its width |
| `mcp_request`, `mcp_tools` | no panic or hang; answers keep their size bounds |

The `Fuzz` workflow runs all 11 every night at 04:23 UTC, 600 seconds per target by default, one job per target, with a 2 GiB RSS limit. It keeps each target's corpus between runs and uploads crashing inputs. Inputs that once failed are kept in `fuzz/regressions/`, and a replay test runs them through the same checks on any OS. `fuzz/run-linux.sh` runs the same targets on a Linux machine for longer, each pinned to its own cores.

Sources: `fuzz/README.md`; `fuzz/fuzz_targets/`; `.github/workflows/fuzz.yml`.

## Miri

The only `unsafe` code in `rift-core` is the counting allocator, and the only other `unsafe` in the binary is two kernel read-ahead hints; the other crates forbid it. Miri runs the `rift-core` library tests every night at 03:41 UTC: most of them under stacked borrows, and the scan, skeleton, moves and key-mode tests under tree borrows, with 8 cases per property test. A failing nightly run opens an issue, or comments on the open one. On a pull request, or a push to any branch but `main`, the CI job `unsafe-diff` looks for `unsafe` in the Rust lines the change adds, and when it finds one the same Miri jobs run as part of CI.

Sources: `.github/workflows/miri.yml`, `.github/workflows/miri-run.yml`, `.github/workflows/ci.yml` (jobs `unsafe-diff` and `miri`); `crates/rift-core/src/alloc.rs`; `crates/rift-cli/src/source.rs`, `hints`.

## Memory, to the byte

Two kinds of test hold the heap to `--mem`. The heap tests in `rift-core/tests` (`scan_heap.rs`, `refine_heap.rs`, `moves_heap.rs`, `unified_heap.rs` and others) read the counting allocator's peak around one operation and hold it to a bound: the reservation or estimate that should cover it, or a few KiB when a tiny pair runs under a 1 TiB budget. Each sits alone in its test binary, since the counter is process-wide.

The CI `memory` job runs on macOS and Linux. `bench/ci-mem.sh` generates a 1 GiB pair, runs `rift --summary`, the full diff and key mode at `--mem 256M`, takes the exact byte count from the `peak heap` line, and fails when it is larger than 268,435,456. It prints the peak resident set next to it.

Sources: `bench/ci-mem.sh`; `.github/workflows/ci.yml`, job `memory`; `crates/rift-core/tests/`.
