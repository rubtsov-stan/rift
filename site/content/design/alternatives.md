+++
title = "Alternatives Rift does not use, and why"
description = "Whole-file Myers, sort-based diffs, mmap, fixed chunks and external-memory schemes, measured against Rift's heap limit and its rule of writing no files."
weight = 8
[extra]
nav = "Alternatives"
h1 = "Alternatives considered"
lead = "Each of these approaches works well somewhere. This page says where, and which of Rift's constraints rules it out for large files."
summary = "Whole-file Myers, sort-based diffs, mmap, fixed chunks and external-memory approaches, against Rift's constraints."
+++

Rift's constraints come from [DESIGN.md](https://github.com/rubtsov-stan/rift/blob/main/DESIGN.md): a heap limit you set and Rift keeps, files read in place and never copied or annotated on disk, an output that is a correct edit script, and a first screen after two passes over the disk.

## Whole-file Myers, as diff and git do

GNU diff and git read both files, split them into lines and run Myers' algorithm, or patience diff, over the whole pair. The output is minimal or close to it, and on files that fit in memory this is the right tool.

The cost is memory in proportion to the files. Even the leanest form needs a hash or an index per line before it can start: for the 10 GiB generated SQL pair, with 131.6 million lines per file, 8-byte hashes alone for both files are 2.1 GB, eight times the default `--mem`. The tools also keep the file contents, so the real figure is larger. From the benchmark runs:

<figure class="diagram">
<svg viewBox="0 0 720 250" role="img" aria-labelledby="alt-t alt-d">
<title id="alt-t">Rift, diff and git diff on a 256 MiB pair</title>
<desc id="alt-d">Bar chart for the 256 MiB SQL pair on Linux. Peak resident memory: rift 112.8 MiB, diff 776.2 MiB, git diff --no-index 1.0 GiB. Wall time: rift 0.28 s, diff 1.61 s, git diff 2.57 s. Rift ran with --mem 256M and peaked at 83.2 MiB of heap.</desc>
<text x="10" y="20" font-weight="700">peak resident memory</text>
<text x="10" y="46" style="font-size:12px">rift</text><rect x="120" y="34" width="52" height="16" style="fill:var(--accent)"/><text x="180" y="46" style="font-size:12px">112.8 MiB</text>
<text x="10" y="72" style="font-size:12px">diff</text><rect x="120" y="60" width="355" height="16" style="fill:var(--muted)"/><text x="483" y="72" style="font-size:12px">776.2 MiB</text>
<text x="10" y="98" style="font-size:12px">git diff</text><rect x="120" y="86" width="469" height="16" style="fill:var(--muted)"/><text x="597" y="98" style="font-size:12px">1.0 GiB</text>
<text x="10" y="140" font-weight="700">wall time</text>
<text x="10" y="166" style="font-size:12px">rift</text><rect x="120" y="154" width="55" height="16" style="fill:var(--accent)"/><text x="183" y="166" style="font-size:12px">0.28 s</text>
<text x="10" y="192" style="font-size:12px">diff</text><rect x="120" y="180" width="313" height="16" style="fill:var(--muted)"/><text x="441" y="192" style="font-size:12px">1.61 s</text>
<text x="10" y="218" style="font-size:12px">git diff</text><rect x="120" y="206" width="500" height="16" style="fill:var(--muted)"/><text x="628" y="218" style="font-size:12px">2.57 s</text><text x="10" y="244" style="font-size:12px">Intel Core i5-10600, Ubuntu 26.04, files in tmpfs, rift v0.1.0</text>
</svg>
<figcaption>Runs 001 to 003 of <code>bench/results/linux-intel-core-i5-10600-2026-11-06/report.md</code>, first of three repeats each; the other two agree within 0.02 s.</figcaption>
</figure>

On the 1 GiB pair the gap decides whether the run happens at all. On Linux under a 3 GiB memory cap, GNU diff was killed (run 030). On the Apple M1, the system diff took 17.55 s and 3.2 GiB, against 0.93 s and 130.8 MiB for Rift (`site/data/bench-mac.json`). git calls any file over 512 MiB binary, so `git diff --no-index` on the 1 GiB pair printed only that the binary files differ, 241 bytes of output, after 42 to 44 s (runs 031 to 035).

Rift keeps Myers, in its linear-space form, for the place where it is cheap: one differing segment at a time, between anchors that are a few hundred lines apart on typical data. What it gives up is minimality across segments. On the 40 small pairs of the differential test against `git diff --patience`, the two changed the same number of lines, 1,241 each; on large pairs Rift's diff depends on the stratum, so it can be longer than a whole-file diff.

Sources: [Refinement](@/design/refinement.md); DESIGN.md, "Spike results" (line counts); `crates/rift-cli/tests/full.rs`, `differential_against_git_patience`.

## Sort-based comparison

Sorting both files and walking them side by side (`sort`, then `comm` or `join`) handles any size in bounded memory: an external sort writes runs that do not fit its buffer to temporary files and merges them.

Two things rule it out as Rift's main mode. First, a sorted comparison is a comparison of sets of lines: it loses the order and, with `uniq`, the repeats, so it cannot produce an edit script, which is Rift's first guarantee. Second, it writes the data again. Each file is sorted to a temporary copy, so you need free disk about the size of the input, and the sort costs time before the comparison starts. Sorting one 256 MiB file of the generator's `shuffled` CSV with `LC_ALL=C sort -S 64M` took 2.14 to 2.29 s on an Apple M1 and peaked at 108 MiB resident; with the default locale it took 16.9 to 17.8 s.

The case where order does not matter, a dump whose rows were reordered, is real. Rift answers it with [key mode](@/design/key-mode.md), which needs a key column instead of a sort: `rift --key csv:id --mem 64M` compared both of those 256 MiB files in one pass, in 0.39 to 0.57 s, and wrote nothing to disk. When the budget is too small for one pass, key mode reads the files again instead of writing anything.

Measured 2026-11-07, Apple M1, 16 GiB, macOS 27.0.1, `sort` 2.3-Apple, warm page cache: `/usr/bin/time -l sort -S 64M -T DIR a > sorted`, three runs with `LC_ALL=C`, two without; the `rift --key` figures are from the key mode page.

## Memory-mapping the files

`mmap` saves a copy and lets the kernel do read-ahead, and many tools that read large files use it. Rift reads with `pread` into fixed buffers instead, for one reason: a page of a mapped file that the process has touched counts in its resident set, so the resident set grows with the file, whatever the heap limit says. Rift promises a bound you can check, and the CI memory job prints the peak resident set next to the heap.

A small experiment shows the size of it. Counting the lines of a 256 MiB file in Python peaked at 270 MiB resident (283,328,512 bytes) through `mmap` and at 15.3 MiB (16,007,168 bytes) through `os.preadv` with a 1 MiB buffer. The method is on the [scanning](@/design/scanning/index.md) page. The kernel can reclaim clean mapped pages under pressure, so this is not a leak, but a container's memory cap or a user reading `top` sees the larger number. A second cost: when another process truncates a mapped file, the next access to a lost page raises `SIGBUS`, where `pread` would return fewer bytes.

The price of `pread` is a copy from the page cache into the buffer, and the DESIGN.md spike puts the scan at disk speed with four or more threads anyway: 3.32 GB/s on four threads against 3.25 GB/s for `cat`, cold cache.

Sources: `crates/rift-cli/src/source.rs`, `FileSource`; `bench/ci-mem.sh`; DESIGN.md, "Memory invariant" and "Spike results".

## Fixed chunks

Cutting both files into chunks of a fixed number of lines or bytes and diffing chunk against chunk keeps memory bounded and is easy. It breaks at the first insertion: every boundary after it shifts, chunk `k` of A no longer lines up with chunk `k` of B, and the diff reports the rest of the file as changed. Rift's sample is chosen by line content, so an insertion moves the sampled lines along with everything else, and both files keep sampling the same lines, as the figure on the [scanning](@/design/scanning/index.md) page shows.

## External-memory approaches

A diff could also write its own data to disk: a line index or a hash table that does not fit in memory, sorted runs of hashes, or an index kept next to the files for the next run. These lift the memory limit without giving up the edit script.

Rift opens both files read-only and writes nothing but its output. Scratch space would need a limit of its own, cleanup after a crash, and a check that it still matches the files. DESIGN.md defers the persistent index for this reason: it "writes files next to user data and needs its own design for safety and invalidation". Without writes, the floor is two reads of each file, and Rift is close to it: on a 10 GiB pair larger than the page cache, `--summary` takes 2.03 to 2.13 times as long as `cat a b` (DESIGN.md, MVP criteria). An external scheme would add at least one write and one read of its own data on top.

The trade Rift makes is time for disk: a smaller `--mem` means a coarser map in line mode and more passes in key mode, never a temporary file.
