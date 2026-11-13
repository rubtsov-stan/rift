+++
title = "The memory budget: two halves and the --mem advice"
description = "What each half of Rift's --mem budget pays for, how the counting allocator checks it, and why the rerun with --mem N advice is a hint, not a minimum."
weight = 4
[extra]
nav = "The memory budget"
h1 = "The memory budget"
lead = "Rift reserves every allocation on its data path from one of two halves of --mem before it makes it. A counting allocator checks the result from the outside."
summary = "The two halves, the counting allocator, how a refusal computes its advice, and where that advice fails."
+++

[DESIGN.md](https://github.com/rubtsov-stan/rift/blob/main/DESIGN.md) states the invariant under "Memory invariant".

## Two halves

With `B` the value of `--mem`, Rift keeps, at every moment:

```
skeleton + index                          <= B / 2
active refinement work + hunk cache       <= B / 2
```

`Budget::new` gives each half `B / 2`. Code that wants memory on the data path calls `try_reserve(half, bytes)` first and gets a `Reservation` or a `BudgetExceeded` error. The counter is a compare-and-swap loop, so it never reads above the capacity, not even for an instant while two threads race. A reservation can grow and shrink, and gives its bytes back when dropped, so a vector and the bytes that pay for it live and die together.

Sources: `crates/rift-core/src/budget.rs`, `Budget::new`, `Budget::try_reserve`, `Inner::add`, `Reservation`.

<figure class="diagram">
<svg viewBox="0 0 720 250" role="img" aria-labelledby="mem-t mem-d">
<title id="mem-t">What each half of the budget pays for</title>
<desc id="mem-d">A bar split in two equal halves. The skeleton half pays, during the scan, for read buffers; then for the samples and line indexes, the anchor table and chain search, the comparison buffers and the segment map; for the life of the diff, the two line indexes, the segment map and the off-chain list. The refine half pays for the front end's readers and output buffer, each worker's readers and line buffers, the moved blocks, and the scheduler: its row table, the hunk cache and the running jobs, which share what is left in two equal parts.</desc>
<rect class="box" x="10" y="20" width="345" height="40"/><rect class="box" x="365" y="20" width="345" height="40" style="stroke:var(--accent)"/>
<text x="20" y="45" font-weight="700">skeleton half, B/2</text><text x="375" y="45" font-weight="700">refine half, B/2</text>
<text x="10" y="86" style="font-size:12px">scan: read buffers (1/4 of the half)</text>
<text x="10" y="106" style="font-size:12px">samples, 24 bytes each</text>
<text x="10" y="126" style="font-size:12px">line indexes, 16 bytes per checkpoint</text>
<text x="10" y="146" style="font-size:12px">anchor table, 16 bytes per slot</text>
<text x="10" y="166" style="font-size:12px">chain search, 12 bytes per candidate</text>
<text x="10" y="186" style="font-size:12px">comparison buffers, up to 1 MiB each</text>
<text x="10" y="206" style="font-size:12px">segment map, 56 bytes per segment</text>
<text x="10" y="226" style="font-size:12px">off-chain pairs, 16 bytes each</text>
<text x="365" y="86" style="font-size:12px">front end: readers, 1 MiB output buffer</text>
<text x="365" y="106" style="font-size:12px">per worker: readers, line buffers</text>
<text x="365" y="126" style="font-size:12px">moved blocks, 32 bytes each</text>
<text x="365" y="146" style="font-size:12px">scheduler row table</text>
<text x="365" y="166" style="font-size:12px">what is left, split in two:</text>
<text x="381" y="186" style="font-size:12px">hunk cache</text>
<text x="381" y="206" style="font-size:12px">running jobs (work_cap)</text>
<text x="365" y="226" style="font-size:12px">key mode: windows, waiting records</text>
</svg>
<figcaption>Key mode uses the same halves differently: its hash tables come from the skeleton half and its read windows and waiting records from the refine half.</figcaption>
</figure>

The skeleton half pays for the map. While a file is scanned, a quarter of the half goes to read buffers. The samples and checkpoints stay until the map is built; the anchor table, the chain search and the comparison buffers come and go during the build. What stays for the life of the diff is the two line indexes, the segment list with each segment's start offsets, and the off-chain list that moved-block detection reads. When something does not fit, the skeleton raises the stratum, which halves the sample, and tries again.

The refine half pays for the work after the map: the front end's two readers and the unified writer's 1 MiB output buffer, then each worker's readers and buffers, then the moved blocks, then the scheduler, which takes all that is left and splits it between the hunk cache and running jobs. The half holds buffer sets for as many workers as fit in half of what it has free, at most eight, whether or not the machine has the cores to start them all, so the scheduler gets the same share on any machine.

Sources: `crates/rift-core/src/scan/mod.rs`, module docs and `ScanConfig::for_pair`; `crates/rift-core/src/skeleton/mod.rs`, module docs, `build_watched`, `SEGMENT_BYTES`; `crates/rift-core/src/diff/mod.rs`, module docs and `Diff::with_watch`; `crates/rift-core/src/sched/mod.rs`, `Scheduler::new`.

## The counting allocator

The budget counts what Rift reserves. Whether the process allocates only that is a separate question, and Rift checks it with a counting allocator. `CountingAlloc` wraps the system allocator, adds every allocation's size to a current counter and keeps the highest value it reached. It is the only `unsafe` code in `rift-core`, and the `rift` binary installs it as the global allocator. `--summary` and `--stats` print the peak, rounded and then exact:

```
peak heap 83.8 MiB of 256.0 MiB (87881880 bytes)
```

That line is from `rift --summary --mem 256M` on a 256 MiB pair of the generator's `shuffled` profile (seed 7), on the Apple M1 described below.

The CI memory job builds the release binary on macOS and on Linux, generates a 1 GiB pair, and runs `--summary`, the full diff and key mode at `--mem 256M`. `bench/ci-mem.sh` takes the exact byte count from that line and fails the job when it exceeds the limit, with no rounding. The heap tests in `rift-core/tests` assert the same counter, one test per test binary, since the counter is process-wide.

The counter measures heap, not the resident set. In the Linux benchmark, the full diff of a 1 GiB pair at `--mem 256M` peaked at 83.6 MiB of heap and 113.0 MiB resident; the difference is the binary, thread stacks and the kernel's bookkeeping, none of which grows with the file.

Sources: `crates/rift-core/src/alloc.rs`; `crates/rift-cli/src/main.rs` (`#[global_allocator]`); `crates/rift-cli/src/summary.rs`, `peak_heap`; `.github/workflows/ci.yml`, job `memory`; `bench/ci-mem.sh`; `bench/results/linux-intel-core-i5-10600-2026-11-06/report.md`, run 025.

## A refusal and its advice

When a reservation fails and no thinning can help, Rift stops with exit status 2 and names a budget to try:

```
rift: skeleton budget exceeded: requested 131587 bytes, 6296 available; rerun with --mem 261K
```

`BudgetExceeded::mem_advice` computes that number from the refusal alone. The half held its capacity minus what was available. To get past this point the half needs what it held plus the request, so the advice is twice that, the two halves being equal. For the line above, at `--mem 16K` each half is 8,192 bytes and held 1,896 of them; `2 × (1,896 + 131,587)` is 266,966 bytes, which `Bytes::to_mem_arg` rounds up to whole KiB: 261K. The same function gives the CLI's advice and the `suggested_mem` of a refusal in `rift serve`.

Sources: `budget.rs`, `BudgetExceeded::mem_advice`; `crates/rift-cli/src/main.rs`, `mem_advice`; `crates/rift-core/src/types.rs`, `Bytes::to_mem_arg`.

## Why the advice is not a minimum

The advice assumes that what the half held stays the same at the larger budget. It does not, because Rift sizes its plan from `--mem`. The doc comment on `mem_advice` says so: the figure is "not a lower bound, since what a half holds depends on `mem` and a smaller budget can take another path, nor always enough, since a later stage may need more."

The test file `crates/rift-cli/tests/mem_advice.rs` uses a pair of 2,000 short lines, every hundredth changed. Run `--summary` on it at each `--mem` from 285K to 372K and the answers are not monotone:

<figure class="diagram">
<svg viewBox="0 0 720 150" role="img" aria-labelledby="swp-t swp-d">
<title id="swp-t">Which --mem values let --summary run on the 2000-line pair</title>
<desc id="swp-d">One cell per --mem value from 285K to 372K. Red cells were refused, green cells ran. Refused: 285K to 296K, 305K to 320K and 352K to 367K. Ran: 297K to 304K, 321K to 351K and 368K to 372K.</desc>
<text x="8" y="26" style="font-size:12px">--mem, in KiB: green ran, red refused</text>
<rect class="df" x="8" y="40" width="7" height="34"/><rect class="df" x="16" y="40" width="7" height="34"/><rect class="df" x="24" y="40" width="7" height="34"/><rect class="df" x="32" y="40" width="7" height="34"/><rect class="df" x="40" y="40" width="7" height="34"/><rect class="df" x="48" y="40" width="7" height="34"/><rect class="df" x="56" y="40" width="7" height="34"/><rect class="df" x="64" y="40" width="7" height="34"/><rect class="df" x="72" y="40" width="7" height="34"/><rect class="df" x="80" y="40" width="7" height="34"/><rect class="df" x="88" y="40" width="7" height="34"/><rect class="df" x="96" y="40" width="7" height="34"/><rect class="eq" x="104" y="40" width="7" height="34"/><rect class="eq" x="112" y="40" width="7" height="34"/><rect class="eq" x="120" y="40" width="7" height="34"/><rect class="eq" x="128" y="40" width="7" height="34"/><rect class="eq" x="136" y="40" width="7" height="34"/><rect class="eq" x="144" y="40" width="7" height="34"/><rect class="eq" x="152" y="40" width="7" height="34"/><rect class="eq" x="160" y="40" width="7" height="34"/><rect class="df" x="168" y="40" width="7" height="34"/><rect class="df" x="176" y="40" width="7" height="34"/><rect class="df" x="184" y="40" width="7" height="34"/><rect class="df" x="192" y="40" width="7" height="34"/><rect class="df" x="200" y="40" width="7" height="34"/><rect class="df" x="208" y="40" width="7" height="34"/><rect class="df" x="216" y="40" width="7" height="34"/><rect class="df" x="224" y="40" width="7" height="34"/><rect class="df" x="232" y="40" width="7" height="34"/><rect class="df" x="240" y="40" width="7" height="34"/><rect class="df" x="248" y="40" width="7" height="34"/><rect class="df" x="256" y="40" width="7" height="34"/><rect class="df" x="264" y="40" width="7" height="34"/><rect class="df" x="272" y="40" width="7" height="34"/><rect class="df" x="280" y="40" width="7" height="34"/><rect class="df" x="288" y="40" width="7" height="34"/><rect class="eq" x="296" y="40" width="7" height="34"/><rect class="eq" x="304" y="40" width="7" height="34"/><rect class="eq" x="312" y="40" width="7" height="34"/><rect class="eq" x="320" y="40" width="7" height="34"/><rect class="eq" x="328" y="40" width="7" height="34"/><rect class="eq" x="336" y="40" width="7" height="34"/><rect class="eq" x="344" y="40" width="7" height="34"/><rect class="eq" x="352" y="40" width="7" height="34"/><rect class="eq" x="360" y="40" width="7" height="34"/><rect class="eq" x="368" y="40" width="7" height="34"/><rect class="eq" x="376" y="40" width="7" height="34"/><rect class="eq" x="384" y="40" width="7" height="34"/><rect class="eq" x="392" y="40" width="7" height="34"/><rect class="eq" x="400" y="40" width="7" height="34"/><rect class="eq" x="408" y="40" width="7" height="34"/><rect class="eq" x="416" y="40" width="7" height="34"/><rect class="eq" x="424" y="40" width="7" height="34"/><rect class="eq" x="432" y="40" width="7" height="34"/><rect class="eq" x="440" y="40" width="7" height="34"/><rect class="eq" x="448" y="40" width="7" height="34"/><rect class="eq" x="456" y="40" width="7" height="34"/><rect class="eq" x="464" y="40" width="7" height="34"/><rect class="eq" x="472" y="40" width="7" height="34"/><rect class="eq" x="480" y="40" width="7" height="34"/><rect class="eq" x="488" y="40" width="7" height="34"/><rect class="eq" x="496" y="40" width="7" height="34"/><rect class="eq" x="504" y="40" width="7" height="34"/><rect class="eq" x="512" y="40" width="7" height="34"/><rect class="eq" x="520" y="40" width="7" height="34"/><rect class="eq" x="528" y="40" width="7" height="34"/><rect class="eq" x="536" y="40" width="7" height="34"/><rect class="df" x="544" y="40" width="7" height="34"/><rect class="df" x="552" y="40" width="7" height="34"/><rect class="df" x="560" y="40" width="7" height="34"/><rect class="df" x="568" y="40" width="7" height="34"/><rect class="df" x="576" y="40" width="7" height="34"/><rect class="df" x="584" y="40" width="7" height="34"/><rect class="df" x="592" y="40" width="7" height="34"/><rect class="df" x="600" y="40" width="7" height="34"/><rect class="df" x="608" y="40" width="7" height="34"/><rect class="df" x="616" y="40" width="7" height="34"/><rect class="df" x="624" y="40" width="7" height="34"/><rect class="df" x="632" y="40" width="7" height="34"/><rect class="df" x="640" y="40" width="7" height="34"/><rect class="df" x="648" y="40" width="7" height="34"/><rect class="df" x="656" y="40" width="7" height="34"/><rect class="df" x="664" y="40" width="7" height="34"/><rect class="eq" x="672" y="40" width="7" height="34"/><rect class="eq" x="680" y="40" width="7" height="34"/><rect class="eq" x="688" y="40" width="7" height="34"/><rect class="eq" x="696" y="40" width="7" height="34"/><rect class="eq" x="704" y="40" width="7" height="34"/>
<path class="link" d="M8 78 V90"/><text x="8" y="106" style="font-size:12px">285</text>
<path class="link" d="M104 78 V90"/><text x="104" y="106" style="font-size:12px">297</text>
<path class="link" d="M168 78 V90"/><text x="168" y="106" style="font-size:12px">305</text>
<path class="link" d="M296 78 V90"/><text x="296" y="106" style="font-size:12px">321</text>
<path class="link" d="M544 78 V90"/><text x="544" y="106" style="font-size:12px">352</text>
<path class="link" d="M672 78 V90"/><text x="672" y="106" style="font-size:12px">368</text>
<text x="8" y="138" style="font-size:12px">a run that works at 304K is refused at 305K</text>
</svg>
<figcaption><code>rift --summary --mem NK a b</code> for every N from 285 to 372, on the pair from <code>mem_advice.rs</code>. Apple M1, 2026-11-07; three repeats of the values at each edge gave the same result.</figcaption>
</figure>

Every refusal in that range asks for 131,587 or 131,667 bytes, the read buffer and staging vectors for scanning one file: the file's length plus one byte, 96 KiB for samples, 16 KiB for checkpoints, 8 bytes for the chunk counter. At 305K the skeleton half (156,160 bytes) had 125,120 free when B's scan asked, so 31,040 bytes were already held, by A's scan result and the start of B's. At 297K the same request succeeded, so at most 20,397 bytes were held. A's sample and checkpoint vectors grow by doubling while the half allows, and when a doubling is refused the scan raises the stratum instead. With a few KiB more, A keeps a finer sample, and B's buffers no longer fit beside it.

Following the advice still gets there. From 16K the run is refused six times, at 16K, 261K, 270K, 282K, 285K and 295K, and runs at 297K. From 305K the advice is 318K, which is refused in turn and advises 321K, which runs. The test `summary_and_full_diff_advise_a_mem_that_works` asserts that property, up to 32 steps, and that every advised value is larger than the one before.

So the honest description of `rerun with --mem N` is a budget past the stage that failed, computed in one step. A run that works at a limit should work at any larger one. The sweep shows a place where it does not, and the step where a larger budget picks the costlier plan is still to be fixed.

Measured 2026-11-07 on an Apple M1, 16 GiB, macOS 27.0.1: `rift --summary --mem NK a b` for N from 285 to 372, where `a` holds `row 0` to `row 1999` and `b` replaces every hundredth line with `changed N`, as `text(false)` and `text(true)` in `mem_advice.rs` build them.

Sources: `crates/rift-cli/tests/mem_advice.rs`; `scan/mod.rs`, `scan_metered` (the `work` reservation), `Shared::push_samples`, `Shared::grow`, `STAGING_SAMPLES`, `STAGING_CHECKPOINTS`.
