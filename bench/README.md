# Benchmarks

`run.sh`, `data.sh` and `ci-mem.sh` are the macOS and CI tools. `linux-safe.sh` is the one to run on a Linux machine you care about.

## linux-safe.sh

```sh
bench/linux-safe.sh --dry-run     # print the plan, change nothing
bench/linux-safe.sh               # 10 s to cancel, then the run; 20 to 60 minutes
bench/linux-safe.sh --yes         # no wait
```

Options: `--ref REF` (tag, branch or commit; default `v0.1.0`), `--repo URL`, `--read-only-pair DIR`.

The result is `rift-bench-linux-<cpu>-<date>.tar.gz`, labelled by CPU model and date, not by hostname, in the current directory: `report.md` (tables), `results.json` (every run, `schema_version` 2) and `raw/` (the `time -v` output, stderr and logs of every run). The first line of the report says the files were in RAM, so the numbers show CPU, memory and the algorithm, not the disk.

Needs: Linux, a non-root user, `cargo` (from rustup), `git`, network access to clone and fetch crates, GNU `time` at `/usr/bin/time`, `awk`, `tar`. `systemd-run --user`, `ionice`, `taskset`, `lscpu` and `systemd-detect-virt` are used when present. No Python.

### What it does to the machine

- Writes one directory in tmpfs, `/dev/shm/rift-bench-<pid>` (or under `$XDG_RUNTIME_DIR`). It checks that the filesystem is tmpfs and has 3 GiB free, and refuses otherwise.
- The clone, the build, `CARGO_HOME`, `CARGO_TARGET_DIR`, the generated files and all logs are in that directory. After the build the binaries are copied out and the target directory, crate cache and clone are deleted.
- A trap on exit, error, Ctrl-C and termination removes the directory. A power loss does the same, since it is RAM.
- The only disk write is the result archive. Outside that, `systemd-run` leaves a few lines in the journal, and cargo is pointed away from `~/.cargo`.
- Does not run as root, call `sudo`, drop caches or change kernel settings. It never installs rustup or a toolchain (`RUSTUP_AUTO_INSTALL=0`); if a ref pins a toolchain you do not have, the build stops. Settings in `~/.cargo/config.toml` do not apply, because `CARGO_HOME` moves.
- Each measured command runs as `systemd-run --user --scope -p MemoryMax=<cap> -p MemorySwapMax=0 nice -n 10 ionice -c3 /usr/bin/time -v ...`. Without a systemd user session it falls back to `prlimit --as=<cap>` (an address-space limit, stricter than RSS) and says so in the report. With neither, `diff` and `git diff` are skipped.
- Caps: 1 GiB for rift, 1.5 GiB for the `--mem 1G` run (its heap alone is 1 GiB; the extra half GiB comes out of the margin below), 3 times the pair for `diff` and `git diff`. `rift-gen` runs under a cap of the pair it writes plus 1 GiB, because files written to tmpfs count against the memory cap of the process that writes them.
- Sizing: the largest pair of 1, 2, 4, 8 or 16 GiB for which `2 x size + 1 GiB cap + 2 GiB margin` stays within 60% of `MemAvailable` and the pair plus 1 GiB fits the tmpfs. Below 8.4 GiB of `MemAvailable` not even a 1 GiB pair fits, and the script stops. Pairs of 1 GiB and of the largest size are run, one pair at a time; the small one is deleted before the large one is generated.
- `diff` and `git diff --no-index` run on the 1 GiB pair when a 3 GiB cap fits the same 60% rule. GNU `diff` needs more than that on a 1 GiB pair, so expect it to be killed there; that is a result, not a failure.
- A separate 256 MiB pair per profile, generated first, gives `rift`, `diff` and `git diff` real times on the same data, with a 2 GiB cap for `diff` and `git diff`.

### What it runs

Seed 7, profiles `sql` and `csv`, changes at 0.1% (`rift-gen --change-ratio 0.001`, also the default). For each profile and size, three repetitions of `cat a b | wc -c` (through a pipe, so the bytes are really read; output bytes confirm the size), `rift --summary --mem 256M` and `rift --stats --mem 256M` (the diff goes through `wc -c`), and one run each of `rift --stats` at `--mem 64M` and `--mem 1G`. On the 256 MiB pair, three repetitions of `rift --stats --mem 256M`, `diff` and `git diff --no-index`. On the 1 GiB pair also `diff` and `git diff --no-index` (three repetitions) and thread scaling: `rift --summary` on 1, 2, 4 and all CPUs.

Per run: wall, user and sys time, max RSS, exit code, rift's peak heap, output bytes, and systemd's result for the run's scope: `oom-kill` when the cap killed it. Each scope gets its own unit name, and the result is read with `systemctl --user show`, which needs no root. Without systemd the result is `n/a`. Once the cap kills a command on a pair, its remaining repetitions are not run and are recorded as `skipped`. Exit code 1 from `rift`, `diff` and `git diff` means the files differ.

Thread count is set with `taskset -c 0-<k-1>`, not `RAYON_NUM_THREADS`: rift sizes its threads from `std::thread::available_parallelism()`, which follows the CPU affinity mask. If your CPU 0 is not in the allowed set, `taskset` fails and the scaling rows show it.

The host section has the CPU model, threads, governor if readable, RAM, kernel, distro, virtualization, the rustc that built rift, the rift commit, and load average before and after. The page cache is reported as it is, never forced; with data in tmpfs it does not apply.

`--read-only-pair DIR` adds `cat`, `rift --summary` and `rift --stats` runs over `DIR/a` and `DIR/b`. The files are only read and nothing is written beside them, so the run gives a disk number if `DIR` is on disk. Pairs on a network file system or a slow disk make `cat` take as long as they take.

### What it does not do

- It does not measure the disk, except through `--read-only-pair`.
- It does not control CPU frequency, other load or turbo; read the governor and load lines before trusting a number.
- It does not compare against other diff tools beyond GNU `diff` and `git diff` on 1 GiB.
- A build of a ref newer than `v0.1.0` uses the toolchain that ref pins; a `v0.1.0` build uses the stable `cargo` of the host, and the report records the rustc.

### Testing the script off Linux

These knobs turn safety checks off. They exist so the plan, sizing, report and cleanup can be exercised on another OS, and a real run must not set them:

| Variable | Effect |
|---|---|
| `RIFT_BENCH_TESTING=1` | skip the Linux, tmpfs and GNU `time` checks |
| `RIFT_BENCH_MEMINFO=FILE` | read `MemAvailable` from `FILE` |
| `RIFT_BENCH_SHM=DIR` | use `DIR` instead of `/dev/shm` |
| `RIFT_BENCH_TIME=PATH` | use `PATH` instead of `/usr/bin/time` |
| `RIFT_BENCH_LIMITER=systemd` | show the plan as if systemd scopes were available |
