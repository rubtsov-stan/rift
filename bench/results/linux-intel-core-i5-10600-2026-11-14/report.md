Files sit in tmpfs (RAM): this run measures CPU, memory and the algorithm, not the SSD.
Exception: the group `disk` ran on a pair stored on disk (ext2/ext3); see the notes.

## Host

- tool: bench/linux-safe.sh schema 3
- host: linux-intel-core-i5-10600-2026-11-14
- date: 2026-11-14T20:09:55Z
- cpu_model: Intel(R) Core(TM) i5-10600 CPU @ 3.30GHz
- cpu_threads: 12
- cpu_cores_per_socket: 6
- cpu_sockets: 1
- cpu_governor: powersave
- mem_total_bytes: 16111542272
- mem_available_bytes: 14110093312
- kernel: 7.0.0-43-generic
- distro: Ubuntu 26.04.1 LTS
- virt: none
- unknown: 
- rustc: rustc 1.99.1 (58cf8f9c3 2026-10-13)
- suite: standard
- reps: 5
- rift_ref: main
- rift_commit: 94bdbb104551b97f34ea86628661d2b0e22df5fa
- scratch_fs: tmpfs, 7.5 GiB free of the mount at start
- limiter: systemd
- disk_pair_fs: ext2/ext3
- disk_pair_device: /dev/sda2
- disk_pair_model: Samsung SSD 860 (ssd)
- disk_pair_size_bytes: 53687091200
- load_before: 1.40 0.50 0.20
- load_after: 8.03 7.31 4.85

## Plan

```
Plan
  MemAvailable      13.1 GiB of 15.0 GiB; 60% budget 7.9 GiB
  scratch           tmpfs /dev/shm (7.5 GiB free)
  source            https://github.com/rubtsov-stan/rift at main
  suite             standard
  repetitions       5 of each repeated run
  memory cap        systemd-run --user scope, MemoryMax, MemorySwapMax=0: rift 1 GiB (1.5 GiB for --mem 1G)
  pair sizes        1.0 GiB 2.0 GiB (A and B each), profiles sql and csv, seed 7, 0.1% changes
  tmpfs peak        4.0 GiB
  diff, git diff    run on the 1.0 GiB pair, cap 3.0 GiB
  comparison        rift, diff and git diff on a 256 MiB pair, cap 2.0 GiB
  thread scaling    taskset -c 0..k-1 for k = 1 2 4 and all 12 CPUs
  on-disk pair      50.0 GiB each of A and B (profile sql, seed 7, 0.1% changes) in /home/stanislav/Projects/rift/disk (ext2/ext3 on /dev/sda2, Samsung SSD 860, ssd)
  disk use          100.0 GiB written, 267.2 GiB free, 110.0 GiB required (pair plus 10.0 GiB margin); deleted afterwards
  disk runs         cat a b | wc -c, rift --summary, rift --stats, all --mem 256M, one run each, cap 1 GiB; diff and git diff skipped: they need several times the pair in memory
  disk time         about 14 to 71 minutes (four passes over the pair at 500 to 100 MB/s, plus the build and the other runs)
  output            rift-bench-linux-intel-core-i5-10600-2026-11-14.tar.gz in the current directory
```

## Notes

- on-disk pair: ext2/ext3 on /dev/sda2, Samsung SSD 860 (ssd). The script cannot drop the page cache without root, so the cache state is whatever the host had: the pair was written just before the first read, so the last pages of B may be cached. A and B together are 100.0 GiB against 15.0 GiB of RAM; that exceeds RAM, so most reads come from the disk. Reads run at idle I/O priority (ionice -c3), which can lengthen them on a busy disk. diff and git diff were not run: GNU diff holds far more than the 1 GiB rift cap for files this size.

## Runs

wall, user and sys in seconds; peak heap is rift's own count; result is systemd's verdict on the run's scope (oom-kill means the cap killed it; skipped means an earlier repetition was killed, so this one was not run; n/a without systemd).

| id | group | profile | size | tool | threads | rep | wall | user | sys | max RSS | peak heap | output | exit | result |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 001 | compare | sql | 256.0 MiB | rift --stats --mem 256M | all | 1 | 0.21 | 0.58 | 0.46 | 108.3 MiB | 79.0 MiB | 5.9 MiB | 1 | success |
| 002 | compare | sql | 256.0 MiB | diff a b | all | 1 | 1.61 | 1.29 | 0.31 | 776.2 MiB |  | 5.2 MiB | 1 | success |
| 003 | compare | sql | 256.0 MiB | git diff --no-index a b | all | 1 | 2.57 | 2.37 | 0.20 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 004 | compare | sql | 256.0 MiB | rift --stats --mem 256M | all | 2 | 0.21 | 0.59 | 0.44 | 108.1 MiB | 79.0 MiB | 5.9 MiB | 1 | success |
| 005 | compare | sql | 256.0 MiB | diff a b | all | 2 | 1.62 | 1.32 | 0.30 | 776.1 MiB |  | 5.2 MiB | 1 | success |
| 006 | compare | sql | 256.0 MiB | git diff --no-index a b | all | 2 | 2.59 | 2.37 | 0.21 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 007 | compare | sql | 256.0 MiB | rift --stats --mem 256M | all | 3 | 0.21 | 0.61 | 0.43 | 108.4 MiB | 79.0 MiB | 5.9 MiB | 1 | success |
| 008 | compare | sql | 256.0 MiB | diff a b | all | 3 | 1.63 | 1.32 | 0.30 | 776.2 MiB |  | 5.2 MiB | 1 | success |
| 009 | compare | sql | 256.0 MiB | git diff --no-index a b | all | 3 | 2.57 | 2.38 | 0.19 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 010 | compare | sql | 256.0 MiB | rift --stats --mem 256M | all | 4 | 0.21 | 0.59 | 0.44 | 108.5 MiB | 79.0 MiB | 5.9 MiB | 1 | success |
| 011 | compare | sql | 256.0 MiB | diff a b | all | 4 | 1.62 | 1.31 | 0.31 | 776.2 MiB |  | 5.2 MiB | 1 | success |
| 012 | compare | sql | 256.0 MiB | git diff --no-index a b | all | 4 | 2.59 | 2.37 | 0.21 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 013 | compare | sql | 256.0 MiB | rift --stats --mem 256M | all | 5 | 0.21 | 0.58 | 0.45 | 108.4 MiB | 79.0 MiB | 5.9 MiB | 1 | success |
| 014 | compare | sql | 256.0 MiB | diff a b | all | 5 | 1.62 | 1.29 | 0.32 | 776.1 MiB |  | 5.2 MiB | 1 | success |
| 015 | compare | sql | 256.0 MiB | git diff --no-index a b | all | 5 | 2.58 | 2.38 | 0.20 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 016 | compare | csv | 256.0 MiB | rift --stats --mem 256M | all | 1 | 0.21 | 0.62 | 0.43 | 106.6 MiB | 78.2 MiB | 5.4 MiB | 1 | success |
| 017 | compare | csv | 256.0 MiB | diff a b | all | 1 | 1.54 | 1.23 | 0.31 | 764.9 MiB |  | 4.7 MiB | 1 | success |
| 018 | compare | csv | 256.0 MiB | git diff --no-index a b | all | 1 | 2.51 | 2.31 | 0.20 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 019 | compare | csv | 256.0 MiB | rift --stats --mem 256M | all | 2 | 0.21 | 0.59 | 0.46 | 106.5 MiB | 78.2 MiB | 5.4 MiB | 1 | success |
| 020 | compare | csv | 256.0 MiB | diff a b | all | 2 | 1.54 | 1.23 | 0.31 | 764.9 MiB |  | 4.7 MiB | 1 | success |
| 021 | compare | csv | 256.0 MiB | git diff --no-index a b | all | 2 | 2.54 | 2.31 | 0.22 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 022 | compare | csv | 256.0 MiB | rift --stats --mem 256M | all | 3 | 0.21 | 0.60 | 0.44 | 106.1 MiB | 78.2 MiB | 5.4 MiB | 1 | success |
| 023 | compare | csv | 256.0 MiB | diff a b | all | 3 | 1.53 | 1.22 | 0.30 | 764.9 MiB |  | 4.7 MiB | 1 | success |
| 024 | compare | csv | 256.0 MiB | git diff --no-index a b | all | 3 | 2.53 | 2.31 | 0.22 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 025 | compare | csv | 256.0 MiB | rift --stats --mem 256M | all | 4 | 0.21 | 0.58 | 0.47 | 106.4 MiB | 78.2 MiB | 5.4 MiB | 1 | success |
| 026 | compare | csv | 256.0 MiB | diff a b | all | 4 | 1.53 | 1.21 | 0.32 | 764.9 MiB |  | 4.7 MiB | 1 | success |
| 027 | compare | csv | 256.0 MiB | git diff --no-index a b | all | 4 | 2.53 | 2.32 | 0.20 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 028 | compare | csv | 256.0 MiB | rift --stats --mem 256M | all | 5 | 0.21 | 0.61 | 0.44 | 106.2 MiB | 78.2 MiB | 5.4 MiB | 1 | success |
| 029 | compare | csv | 256.0 MiB | diff a b | all | 5 | 1.55 | 1.24 | 0.30 | 764.9 MiB |  | 4.7 MiB | 1 | success |
| 030 | compare | csv | 256.0 MiB | git diff --no-index a b | all | 5 | 2.51 | 2.31 | 0.20 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 031 | generated | sql | 1.0 GiB | cat a b | wc -c | all | 1 | 0.05 | 0.00 | 0.05 | 7.6 MiB |  | 2.0 GiB | 0 | success |
| 032 | generated | sql | 1.0 GiB | cat a b | wc -c | all | 2 | 0.05 | 0.00 | 0.05 | 7.7 MiB |  | 2.0 GiB | 0 | success |
| 033 | generated | sql | 1.0 GiB | cat a b | wc -c | all | 3 | 0.05 | 0.00 | 0.05 | 7.5 MiB |  | 2.0 GiB | 0 | success |
| 034 | generated | sql | 1.0 GiB | cat a b | wc -c | all | 4 | 0.05 | 0.00 | 0.05 | 7.5 MiB |  | 2.0 GiB | 0 | success |
| 035 | generated | sql | 1.0 GiB | cat a b | wc -c | all | 5 | 0.04 | 0.00 | 0.04 | 7.5 MiB |  | 2.0 GiB | 0 | success |
| 036 | generated | sql | 1.0 GiB | rift --summary --mem 256M | all | 1 | 0.45 | 1.34 | 1.47 | 109.1 MiB | 79.4 MiB | 1.0 KiB | 1 | success |
| 037 | generated | sql | 1.0 GiB | rift --summary --mem 256M | all | 2 | 0.45 | 1.33 | 1.48 | 109.4 MiB | 79.4 MiB | 1.0 KiB | 1 | success |
| 038 | generated | sql | 1.0 GiB | rift --summary --mem 256M | all | 3 | 0.46 | 1.32 | 1.49 | 109.3 MiB | 79.4 MiB | 1.0 KiB | 1 | success |
| 039 | generated | sql | 1.0 GiB | rift --summary --mem 256M | all | 4 | 0.46 | 1.31 | 1.50 | 109.5 MiB | 79.4 MiB | 1.0 KiB | 1 | success |
| 040 | generated | sql | 1.0 GiB | rift --summary --mem 256M | all | 5 | 0.45 | 1.33 | 1.48 | 109.2 MiB | 79.4 MiB | 1.0 KiB | 1 | success |
| 041 | generated | sql | 1.0 GiB | rift --stats --mem 256M | all | 1 | 0.53 | 1.46 | 1.56 | 109.3 MiB | 79.4 MiB | 23.0 MiB | 1 | success |
| 042 | generated | sql | 1.0 GiB | rift --stats --mem 256M | all | 2 | 0.52 | 1.42 | 1.59 | 109.4 MiB | 79.4 MiB | 23.0 MiB | 1 | success |
| 043 | generated | sql | 1.0 GiB | rift --stats --mem 256M | all | 3 | 0.53 | 1.45 | 1.58 | 109.5 MiB | 79.4 MiB | 23.0 MiB | 1 | success |
| 044 | generated | sql | 1.0 GiB | rift --stats --mem 256M | all | 4 | 0.52 | 1.42 | 1.60 | 109.2 MiB | 79.4 MiB | 23.0 MiB | 1 | success |
| 045 | generated | sql | 1.0 GiB | rift --stats --mem 256M | all | 5 | 0.53 | 1.40 | 1.62 | 109.2 MiB | 79.4 MiB | 23.0 MiB | 1 | success |
| 046 | generated | sql | 1.0 GiB | rift --stats --mem 64M | all | 1 | 0.77 | 0.71 | 0.73 | 26.7 MiB | 19.0 MiB | 23.1 MiB | 1 | success |
| 047 | generated | sql | 1.0 GiB | rift --stats --mem 1G | all | 1 | 0.86 | 2.83 | 1.81 | 434.9 MiB | 328.5 MiB | 23.0 MiB | 1 | success |
| 048 | generated | sql | 1.0 GiB | diff a b | all | 1 |  |  |  |  |  | 0 B |  | oom-kill |
| 049 | generated | sql | 1.0 GiB | git diff --no-index a b | all | 1 | 42.55 | 42.33 | 0.21 | 5.6 MiB |  | 201 B | 1 | success |
| 050 | generated | sql | 1.0 GiB | diff a b | all | 2 |  |  |  |  |  |  |  | skipped |
| 051 | generated | sql | 1.0 GiB | git diff --no-index a b | all | 2 | 42.66 | 42.31 | 0.31 | 5.3 MiB |  | 201 B | 1 | success |
| 052 | generated | sql | 1.0 GiB | diff a b | all | 3 |  |  |  |  |  |  |  | skipped |
| 053 | generated | sql | 1.0 GiB | git diff --no-index a b | all | 3 | 42.62 | 42.40 | 0.21 | 5.2 MiB |  | 201 B | 1 | success |
| 054 | generated | sql | 1.0 GiB | diff a b | all | 4 |  |  |  |  |  |  |  | skipped |
| 055 | generated | sql | 1.0 GiB | git diff --no-index a b | all | 4 | 43.77 | 43.55 | 0.21 | 5.4 MiB |  | 201 B | 1 | success |
| 056 | generated | sql | 1.0 GiB | diff a b | all | 5 |  |  |  |  |  |  |  | skipped |
| 057 | generated | sql | 1.0 GiB | git diff --no-index a b | all | 5 | 42.74 | 42.54 | 0.20 | 5.5 MiB |  | 201 B | 1 | success |
| 058 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 1 | 1 | 1.04 | 0.62 | 0.41 | 83.7 MiB | 76.0 MiB | 1.0 KiB | 1 | success |
| 059 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 1 | 2 | 1.04 | 0.63 | 0.40 | 83.8 MiB | 76.0 MiB | 1.0 KiB | 1 | success |
| 060 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 1 | 3 | 1.04 | 0.62 | 0.41 | 83.8 MiB | 76.0 MiB | 1.0 KiB | 1 | success |
| 061 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 1 | 4 | 1.04 | 0.61 | 0.43 | 83.7 MiB | 76.0 MiB | 1.0 KiB | 1 | success |
| 062 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 1 | 5 | 1.04 | 0.61 | 0.43 | 83.8 MiB | 76.0 MiB | 1.0 KiB | 1 | success |
| 063 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 2 | 1 | 0.62 | 0.71 | 0.47 | 88.1 MiB | 76.0 MiB | 1.0 KiB | 1 | success |
| 064 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 2 | 2 | 0.63 | 0.70 | 0.49 | 88.1 MiB | 76.0 MiB | 1.0 KiB | 1 | success |
| 065 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 2 | 3 | 0.62 | 0.70 | 0.48 | 88.1 MiB | 76.0 MiB | 1.0 KiB | 1 | success |
| 066 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 2 | 4 | 0.63 | 0.70 | 0.49 | 88.1 MiB | 76.0 MiB | 1.0 KiB | 1 | success |
| 067 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 2 | 5 | 0.62 | 0.69 | 0.49 | 88.3 MiB | 76.0 MiB | 1.0 KiB | 1 | success |
| 068 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 4 | 1 | 0.47 | 0.79 | 0.88 | 96.6 MiB | 76.0 MiB | 1.0 KiB | 1 | success |
| 069 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 4 | 2 | 0.46 | 0.82 | 0.84 | 96.5 MiB | 76.0 MiB | 1.0 KiB | 1 | success |
| 070 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 4 | 3 | 0.47 | 0.81 | 0.86 | 96.6 MiB | 76.0 MiB | 1.0 KiB | 1 | success |
| 071 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 4 | 4 | 0.46 | 0.82 | 0.83 | 96.6 MiB | 76.0 MiB | 1.0 KiB | 1 | success |
| 072 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 4 | 5 | 0.47 | 0.83 | 0.85 | 96.6 MiB | 76.0 MiB | 1.0 KiB | 1 | success |
| 073 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | all | 1 | 0.46 | 1.34 | 1.52 | 109.2 MiB | 79.4 MiB | 1.0 KiB | 1 | success |
| 074 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | all | 2 | 0.46 | 1.38 | 1.47 | 109.3 MiB | 79.4 MiB | 1.0 KiB | 1 | success |
| 075 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | all | 3 | 0.47 | 1.43 | 1.48 | 109.1 MiB | 79.4 MiB | 1.0 KiB | 1 | success |
| 076 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | all | 4 | 0.48 | 1.51 | 1.47 | 109.5 MiB | 79.4 MiB | 1.0 KiB | 1 | success |
| 077 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | all | 5 | 0.45 | 1.38 | 1.43 | 109.4 MiB | 79.4 MiB | 1.0 KiB | 1 | success |
| 078 | generated | csv | 1.0 GiB | cat a b | wc -c | all | 1 | 0.05 | 0.00 | 0.05 | 7.5 MiB |  | 2.0 GiB | 0 | success |
| 079 | generated | csv | 1.0 GiB | cat a b | wc -c | all | 2 | 0.05 | 0.00 | 0.05 | 7.5 MiB |  | 2.0 GiB | 0 | success |
| 080 | generated | csv | 1.0 GiB | cat a b | wc -c | all | 3 | 0.05 | 0.00 | 0.05 | 7.5 MiB |  | 2.0 GiB | 0 | success |
| 081 | generated | csv | 1.0 GiB | cat a b | wc -c | all | 4 | 0.05 | 0.00 | 0.05 | 7.5 MiB |  | 2.0 GiB | 0 | success |
| 082 | generated | csv | 1.0 GiB | cat a b | wc -c | all | 5 | 0.05 | 0.00 | 0.04 | 7.6 MiB |  | 2.0 GiB | 0 | success |
| 083 | generated | csv | 1.0 GiB | rift --summary --mem 256M | all | 1 | 0.46 | 1.35 | 1.50 | 107.8 MiB | 78.8 MiB | 1.0 KiB | 1 | success |
| 084 | generated | csv | 1.0 GiB | rift --summary --mem 256M | all | 2 | 0.46 | 1.38 | 1.47 | 107.8 MiB | 78.8 MiB | 1.0 KiB | 1 | success |
| 085 | generated | csv | 1.0 GiB | rift --summary --mem 256M | all | 3 | 0.46 | 1.46 | 1.40 | 107.9 MiB | 78.8 MiB | 1.0 KiB | 1 | success |
| 086 | generated | csv | 1.0 GiB | rift --summary --mem 256M | all | 4 | 0.46 | 1.36 | 1.50 | 107.7 MiB | 78.8 MiB | 1.0 KiB | 1 | success |
| 087 | generated | csv | 1.0 GiB | rift --summary --mem 256M | all | 5 | 0.46 | 1.35 | 1.50 | 107.8 MiB | 78.8 MiB | 1.0 KiB | 1 | success |
| 088 | generated | csv | 1.0 GiB | rift --stats --mem 256M | all | 1 | 0.53 | 1.46 | 1.59 | 108.0 MiB | 78.8 MiB | 23.5 MiB | 1 | success |
| 089 | generated | csv | 1.0 GiB | rift --stats --mem 256M | all | 2 | 0.52 | 1.48 | 1.58 | 107.9 MiB | 78.8 MiB | 23.5 MiB | 1 | success |
| 090 | generated | csv | 1.0 GiB | rift --stats --mem 256M | all | 3 | 0.52 | 1.46 | 1.58 | 107.7 MiB | 78.8 MiB | 23.5 MiB | 1 | success |
| 091 | generated | csv | 1.0 GiB | rift --stats --mem 256M | all | 4 | 0.52 | 1.46 | 1.59 | 107.9 MiB | 78.8 MiB | 23.5 MiB | 1 | success |
| 092 | generated | csv | 1.0 GiB | rift --stats --mem 256M | all | 5 | 0.53 | 1.40 | 1.65 | 107.7 MiB | 78.8 MiB | 23.5 MiB | 1 | success |
| 093 | generated | csv | 1.0 GiB | rift --stats --mem 64M | all | 1 | 0.73 | 0.65 | 0.76 | 26.7 MiB | 18.6 MiB | 23.6 MiB | 1 | success |
| 094 | generated | csv | 1.0 GiB | rift --stats --mem 1G | all | 1 | 0.86 | 2.79 | 1.84 | 428.3 MiB | 326.5 MiB | 23.4 MiB | 1 | success |
| 095 | generated | csv | 1.0 GiB | diff a b | all | 1 |  |  |  |  |  | 0 B |  | oom-kill |
| 096 | generated | csv | 1.0 GiB | git diff --no-index a b | all | 1 | 94.74 | 94.51 | 0.22 | 5.3 MiB |  | 201 B | 1 | success |
| 097 | generated | csv | 1.0 GiB | diff a b | all | 2 |  |  |  |  |  |  |  | skipped |
| 098 | generated | csv | 1.0 GiB | git diff --no-index a b | all | 2 | 99.28 | 99.05 | 0.23 | 5.5 MiB |  | 201 B | 1 | success |
| 099 | generated | csv | 1.0 GiB | diff a b | all | 3 |  |  |  |  |  |  |  | skipped |
| 100 | generated | csv | 1.0 GiB | git diff --no-index a b | all | 3 | 96.47 | 96.25 | 0.22 | 5.6 MiB |  | 201 B | 1 | success |
| 101 | generated | csv | 1.0 GiB | diff a b | all | 4 |  |  |  |  |  |  |  | skipped |
| 102 | generated | csv | 1.0 GiB | git diff --no-index a b | all | 4 | 95.14 | 94.91 | 0.22 | 5.5 MiB |  | 201 B | 1 | success |
| 103 | generated | csv | 1.0 GiB | diff a b | all | 5 |  |  |  |  |  |  |  | skipped |
| 104 | generated | csv | 1.0 GiB | git diff --no-index a b | all | 5 | 109.39 | 109.14 | 0.24 | 5.4 MiB |  | 201 B | 1 | success |
| 105 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 1 | 1 | 0.99 | 0.56 | 0.43 | 81.8 MiB | 74.1 MiB | 1.0 KiB | 1 | success |
| 106 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 1 | 2 | 0.99 | 0.55 | 0.43 | 81.8 MiB | 74.1 MiB | 1.0 KiB | 1 | success |
| 107 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 1 | 3 | 0.99 | 0.55 | 0.44 | 81.8 MiB | 74.1 MiB | 1.0 KiB | 1 | success |
| 108 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 1 | 4 | 0.99 | 0.56 | 0.42 | 81.9 MiB | 74.1 MiB | 1.0 KiB | 1 | success |
| 109 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 1 | 5 | 0.99 | 0.56 | 0.42 | 82.0 MiB | 74.1 MiB | 1.0 KiB | 1 | success |
| 110 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 2 | 1 | 0.6 | 0.63 | 0.51 | 86.4 MiB | 74.1 MiB | 1.0 KiB | 1 | success |
| 111 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 2 | 2 | 0.6 | 0.64 | 0.50 | 86.1 MiB | 74.1 MiB | 1.0 KiB | 1 | success |
| 112 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 2 | 3 | 0.6 | 0.63 | 0.50 | 86.2 MiB | 74.1 MiB | 1.0 KiB | 1 | success |
| 113 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 2 | 4 | 0.6 | 0.65 | 0.49 | 86.3 MiB | 74.1 MiB | 1.0 KiB | 1 | success |
| 114 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 2 | 5 | 0.6 | 0.61 | 0.52 | 86.1 MiB | 74.1 MiB | 1.0 KiB | 1 | success |
| 115 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 4 | 1 | 0.46 | 0.79 | 0.85 | 94.4 MiB | 74.1 MiB | 1.0 KiB | 1 | success |
| 116 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 4 | 2 | 0.45 | 0.78 | 0.86 | 94.5 MiB | 74.1 MiB | 1.0 KiB | 1 | success |
| 117 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 4 | 3 | 0.46 | 0.77 | 0.87 | 94.7 MiB | 74.1 MiB | 1.0 KiB | 1 | success |
| 118 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 4 | 4 | 0.45 | 0.79 | 0.83 | 94.7 MiB | 74.1 MiB | 1.0 KiB | 1 | success |
| 119 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 4 | 5 | 0.45 | 0.80 | 0.82 | 94.8 MiB | 74.1 MiB | 1.0 KiB | 1 | success |
| 120 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | all | 1 | 0.46 | 1.34 | 1.50 | 107.7 MiB | 78.8 MiB | 1.0 KiB | 1 | success |
| 121 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | all | 2 | 0.46 | 1.37 | 1.47 | 107.8 MiB | 78.8 MiB | 1.0 KiB | 1 | success |
| 122 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | all | 3 | 0.46 | 1.38 | 1.48 | 107.8 MiB | 78.8 MiB | 1.0 KiB | 1 | success |
| 123 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | all | 4 | 0.46 | 1.38 | 1.47 | 107.8 MiB | 78.8 MiB | 1.0 KiB | 1 | success |
| 124 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | all | 5 | 0.46 | 1.37 | 1.49 | 107.8 MiB | 78.8 MiB | 1.0 KiB | 1 | success |
| 125 | generated | sql | 2.0 GiB | cat a b | wc -c | all | 1 | 0.09 | 0.00 | 0.09 | 7.6 MiB |  | 4.0 GiB | 0 | success |
| 126 | generated | sql | 2.0 GiB | cat a b | wc -c | all | 2 | 0.09 | 0.00 | 0.09 | 7.7 MiB |  | 4.0 GiB | 0 | success |
| 127 | generated | sql | 2.0 GiB | cat a b | wc -c | all | 3 | 0.09 | 0.00 | 0.09 | 7.6 MiB |  | 4.0 GiB | 0 | success |
| 128 | generated | sql | 2.0 GiB | cat a b | wc -c | all | 4 | 0.09 | 0.00 | 0.09 | 7.6 MiB |  | 4.0 GiB | 0 | success |
| 129 | generated | sql | 2.0 GiB | cat a b | wc -c | all | 5 | 0.08 | 0.00 | 0.08 | 7.5 MiB |  | 4.0 GiB | 0 | success |
| 130 | generated | sql | 2.0 GiB | rift --summary --mem 256M | all | 1 | 0.79 | 2.28 | 2.90 | 108.9 MiB | 79.4 MiB | 1.0 KiB | 1 | success |
| 131 | generated | sql | 2.0 GiB | rift --summary --mem 256M | all | 2 | 0.79 | 2.30 | 2.86 | 109.0 MiB | 79.4 MiB | 1.0 KiB | 1 | success |
| 132 | generated | sql | 2.0 GiB | rift --summary --mem 256M | all | 3 | 0.8 | 2.29 | 2.90 | 109.1 MiB | 79.4 MiB | 1.0 KiB | 1 | success |
| 133 | generated | sql | 2.0 GiB | rift --summary --mem 256M | all | 4 | 0.8 | 2.34 | 2.85 | 109.2 MiB | 79.4 MiB | 1.0 KiB | 1 | success |
| 134 | generated | sql | 2.0 GiB | rift --summary --mem 256M | all | 5 | 0.8 | 2.25 | 2.94 | 109.3 MiB | 79.4 MiB | 1.0 KiB | 1 | success |
| 135 | generated | sql | 2.0 GiB | rift --stats --mem 256M | all | 1 | 0.93 | 2.57 | 3.10 | 109.0 MiB | 79.4 MiB | 45.6 MiB | 1 | success |
| 136 | generated | sql | 2.0 GiB | rift --stats --mem 256M | all | 2 | 0.93 | 2.54 | 3.11 | 109.1 MiB | 79.4 MiB | 45.6 MiB | 1 | success |
| 137 | generated | sql | 2.0 GiB | rift --stats --mem 256M | all | 3 | 0.93 | 2.60 | 3.04 | 109.2 MiB | 79.4 MiB | 45.6 MiB | 1 | success |
| 138 | generated | sql | 2.0 GiB | rift --stats --mem 256M | all | 4 | 0.93 | 2.53 | 3.12 | 109.6 MiB | 79.4 MiB | 45.6 MiB | 1 | success |
| 139 | generated | sql | 2.0 GiB | rift --stats --mem 256M | all | 5 | 0.93 | 2.61 | 3.05 | 109.0 MiB | 79.4 MiB | 45.6 MiB | 1 | success |
| 140 | generated | sql | 2.0 GiB | rift --stats --mem 64M | all | 1 | 1.5 | 1.54 | 1.42 | 26.8 MiB | 19.1 MiB | 46.5 MiB | 1 | success |
| 141 | generated | sql | 2.0 GiB | rift --stats --mem 1G | all | 1 | 1.28 | 4.17 | 3.35 | 435.7 MiB | 329.2 MiB | 45.6 MiB | 1 | success |
| 142 | generated | csv | 2.0 GiB | cat a b | wc -c | all | 1 | 0.08 | 0.00 | 0.08 | 7.6 MiB |  | 4.0 GiB | 0 | success |
| 143 | generated | csv | 2.0 GiB | cat a b | wc -c | all | 2 | 0.09 | 0.00 | 0.09 | 7.5 MiB |  | 4.0 GiB | 0 | success |
| 144 | generated | csv | 2.0 GiB | cat a b | wc -c | all | 3 | 0.09 | 0.00 | 0.09 | 7.8 MiB |  | 4.0 GiB | 0 | success |
| 145 | generated | csv | 2.0 GiB | cat a b | wc -c | all | 4 | 0.08 | 0.00 | 0.08 | 7.6 MiB |  | 4.0 GiB | 0 | success |
| 146 | generated | csv | 2.0 GiB | cat a b | wc -c | all | 5 | 0.08 | 0.00 | 0.08 | 7.7 MiB |  | 4.0 GiB | 0 | success |
| 147 | generated | csv | 2.0 GiB | rift --summary --mem 256M | all | 1 | 0.8 | 2.34 | 2.88 | 107.5 MiB | 78.8 MiB | 1.0 KiB | 1 | success |
| 148 | generated | csv | 2.0 GiB | rift --summary --mem 256M | all | 2 | 0.8 | 2.41 | 2.83 | 107.9 MiB | 78.8 MiB | 1.0 KiB | 1 | success |
| 149 | generated | csv | 2.0 GiB | rift --summary --mem 256M | all | 3 | 0.8 | 2.38 | 2.84 | 107.9 MiB | 78.8 MiB | 1.0 KiB | 1 | success |
| 150 | generated | csv | 2.0 GiB | rift --summary --mem 256M | all | 4 | 0.8 | 2.32 | 2.91 | 107.6 MiB | 78.8 MiB | 1.0 KiB | 1 | success |
| 151 | generated | csv | 2.0 GiB | rift --summary --mem 256M | all | 5 | 0.8 | 2.30 | 2.95 | 107.8 MiB | 78.8 MiB | 1.0 KiB | 1 | success |
| 152 | generated | csv | 2.0 GiB | rift --stats --mem 256M | all | 1 | 0.93 | 2.59 | 3.08 | 107.7 MiB | 78.8 MiB | 46.6 MiB | 1 | success |
| 153 | generated | csv | 2.0 GiB | rift --stats --mem 256M | all | 2 | 0.92 | 2.63 | 3.05 | 107.7 MiB | 78.8 MiB | 46.6 MiB | 1 | success |
| 154 | generated | csv | 2.0 GiB | rift --stats --mem 256M | all | 3 | 0.93 | 2.59 | 3.06 | 107.9 MiB | 78.8 MiB | 46.6 MiB | 1 | success |
| 155 | generated | csv | 2.0 GiB | rift --stats --mem 256M | all | 4 | 0.93 | 2.64 | 3.06 | 107.8 MiB | 78.8 MiB | 46.6 MiB | 1 | success |
| 156 | generated | csv | 2.0 GiB | rift --stats --mem 256M | all | 5 | 0.93 | 2.61 | 3.10 | 107.8 MiB | 78.8 MiB | 46.6 MiB | 1 | success |
| 157 | generated | csv | 2.0 GiB | rift --stats --mem 64M | all | 1 | 1.38 | 1.42 | 1.42 | 26.5 MiB | 18.6 MiB | 47.0 MiB | 1 | success |
| 158 | generated | csv | 2.0 GiB | rift --stats --mem 1G | all | 1 | 1.28 | 4.15 | 3.38 | 429.2 MiB | 327.4 MiB | 46.5 MiB | 1 | success |
| 159 | disk | sql | 50.0 GiB | cat a b | wc -c | all | 1 | 218.68 | 0.07 | 7.54 | 7.5 MiB |  | 100.0 GiB | 0 | success |
| 160 | disk | sql | 50.0 GiB | rift --summary --mem 256M | all | 1 | 299.76 | 25.08 | 72.28 | 107.4 MiB | 69.8 MiB | 1.1 KiB | 1 | success |
| 161 | disk | sql | 50.0 GiB | rift --stats --mem 256M | all | 1 | 456.76 | 75.20 | 132.51 | 112.8 MiB | 80.6 MiB | 1.2 GiB | 1 | success |
