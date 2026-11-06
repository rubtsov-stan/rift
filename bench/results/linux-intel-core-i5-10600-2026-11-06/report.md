Files sit in tmpfs (RAM): this run measures CPU, memory and the algorithm, not the SSD.

## Host

- tool: bench/linux-safe.sh schema 1
- host: linux-intel-core-i5-10600-2026-11-06
- date: 2026-11-06T00:07:31Z
- cpu_model: Intel(R) Core(TM) i5-10600 CPU @ 3.30GHz
- cpu_threads: 12
- cpu_cores_per_socket: 6
- cpu_sockets: 1
- cpu_governor: powersave
- mem_total_bytes: 16111542272
- mem_available_bytes: 14278590464
- kernel: 7.0.0-42-generic
- distro: Ubuntu 26.04.1 LTS
- virt: none
- unknown: 
- rustc: rustc 1.99.1 (58cf8f9c3 2026-10-13)
- rift_ref: v0.1.0
- rift_commit: 11586426124eb83f5631d11f55b556aa0a283418
- scratch_fs: tmpfs, 7.5 GiB free of the mount at start
- limiter: systemd
- load_before: 0.68 0.36 0.37
- load_after: 3.32 1.57 0.95

## Plan

```
Plan
  MemAvailable      13.3 GiB of 15.0 GiB; 60% budget 8.0 GiB
  scratch           tmpfs /dev/shm (7.5 GiB free)
  source            https://github.com/rubtsov-stan/rift at v0.1.0
  pair sizes        1.0 GiB 2.0 GiB (A and B each), profiles sql and csv, seed 7, 0.1% changes
  tmpfs peak        4.0 GiB
  memory cap        systemd-run --user scope, MemoryMax, MemorySwapMax=0: rift 1 GiB (1.5 GiB for --mem 1G)
  diff, git diff    run on the 1.0 GiB pair, cap 3.0 GiB
  comparison        rift, diff and git diff on a 256 MiB pair, cap 2.0 GiB
  thread scaling    taskset -c 0..k-1 for k = 1 2 4 and all 12 CPUs
  output            rift-bench-linux-intel-core-i5-10600-2026-11-06.tar.gz in the current directory
```

## Runs

wall, user and sys in seconds; peak heap is rift's own count; result is systemd's verdict on the run's scope (oom-kill means the cap killed it; skipped means an earlier repetition was killed, so this one was not run; n/a without systemd).

| id | group | profile | size | tool | threads | rep | wall | user | sys | max RSS | peak heap | output | exit | result |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 001 | compare | sql | 256.0 MiB | rift --stats --mem 256M | all | 1 | 0.29 | 0.63 | 0.69 | 112.8 MiB | 83.2 MiB | 5.9 MiB | 1 | success |
| 002 | compare | sql | 256.0 MiB | diff a b | all | 1 | 1.61 | 1.29 | 0.32 | 776.2 MiB |  | 5.2 MiB | 1 | success |
| 003 | compare | sql | 256.0 MiB | git diff --no-index a b | all | 1 | 2.57 | 2.37 | 0.20 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 004 | compare | sql | 256.0 MiB | rift --stats --mem 256M | all | 2 | 0.28 | 0.64 | 0.67 | 112.9 MiB | 83.2 MiB | 5.9 MiB | 1 | success |
| 005 | compare | sql | 256.0 MiB | diff a b | all | 2 | 1.63 | 1.33 | 0.30 | 776.3 MiB |  | 5.2 MiB | 1 | success |
| 006 | compare | sql | 256.0 MiB | git diff --no-index a b | all | 2 | 2.57 | 2.37 | 0.20 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 007 | compare | sql | 256.0 MiB | rift --stats --mem 256M | all | 3 | 0.28 | 0.66 | 0.66 | 112.6 MiB | 83.2 MiB | 5.9 MiB | 1 | success |
| 008 | compare | sql | 256.0 MiB | diff a b | all | 3 | 1.62 | 1.31 | 0.30 | 776.3 MiB |  | 5.2 MiB | 1 | success |
| 009 | compare | sql | 256.0 MiB | git diff --no-index a b | all | 3 | 2.58 | 2.38 | 0.20 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 010 | compare | csv | 256.0 MiB | rift --stats --mem 256M | all | 1 | 0.28 | 0.64 | 0.68 | 109.9 MiB | 82.4 MiB | 5.4 MiB | 1 | success |
| 011 | compare | csv | 256.0 MiB | diff a b | all | 1 | 1.53 | 1.23 | 0.30 | 764.9 MiB |  | 4.7 MiB | 1 | success |
| 012 | compare | csv | 256.0 MiB | git diff --no-index a b | all | 1 | 2.53 | 2.32 | 0.21 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 013 | compare | csv | 256.0 MiB | rift --stats --mem 256M | all | 2 | 0.28 | 0.66 | 0.67 | 110.0 MiB | 82.4 MiB | 5.4 MiB | 1 | success |
| 014 | compare | csv | 256.0 MiB | diff a b | all | 2 | 1.53 | 1.21 | 0.32 | 764.9 MiB |  | 4.7 MiB | 1 | success |
| 015 | compare | csv | 256.0 MiB | git diff --no-index a b | all | 2 | 2.55 | 2.34 | 0.20 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 016 | compare | csv | 256.0 MiB | rift --stats --mem 256M | all | 3 | 0.28 | 0.64 | 0.68 | 109.8 MiB | 82.4 MiB | 5.4 MiB | 1 | success |
| 017 | compare | csv | 256.0 MiB | diff a b | all | 3 | 1.53 | 1.21 | 0.31 | 764.9 MiB |  | 4.7 MiB | 1 | success |
| 018 | compare | csv | 256.0 MiB | git diff --no-index a b | all | 3 | 2.51 | 2.33 | 0.18 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 019 | generated | sql | 1.0 GiB | cat a b | all | 1 | 0.05 | 0.00 | 0.05 | 7.4 MiB |  |  | 0 | success |
| 020 | generated | sql | 1.0 GiB | cat a b | all | 2 | 0.05 | 0.00 | 0.05 | 7.5 MiB |  |  | 0 | success |
| 021 | generated | sql | 1.0 GiB | cat a b | all | 3 | 0.05 | 0.00 | 0.05 | 7.5 MiB |  |  | 0 | success |
| 022 | generated | sql | 1.0 GiB | rift --summary --mem 256M | all | 1 | 0.51 | 1.58 | 1.97 | 113.0 MiB | 83.6 MiB | 673 B | 1 | success |
| 023 | generated | sql | 1.0 GiB | rift --summary --mem 256M | all | 2 | 0.51 | 1.59 | 1.97 | 113.0 MiB | 83.6 MiB | 673 B | 1 | success |
| 024 | generated | sql | 1.0 GiB | rift --summary --mem 256M | all | 3 | 0.51 | 1.54 | 2.02 | 113.1 MiB | 83.6 MiB | 673 B | 1 | success |
| 025 | generated | sql | 1.0 GiB | rift --stats --mem 256M | all | 1 | 0.69 | 1.99 | 2.51 | 113.0 MiB | 83.6 MiB | 23.0 MiB | 1 | success |
| 026 | generated | sql | 1.0 GiB | rift --stats --mem 256M | all | 2 | 0.69 | 1.88 | 2.61 | 112.8 MiB | 83.6 MiB | 23.0 MiB | 1 | success |
| 027 | generated | sql | 1.0 GiB | rift --stats --mem 256M | all | 3 | 0.69 | 1.96 | 2.55 | 113.0 MiB | 83.6 MiB | 23.0 MiB | 1 | success |
| 028 | generated | sql | 1.0 GiB | rift --stats --mem 64M | all | 1 | 0.54 | 0.77 | 1.11 | 30.7 MiB | 21.0 MiB | 23.1 MiB | 1 | success |
| 029 | generated | sql | 1.0 GiB | rift --stats --mem 1G | all | 1 | 1.2 | 3.06 | 3.82 | 436.3 MiB | 329.2 MiB | 23.0 MiB | 1 | success |
| 030 | generated | sql | 1.0 GiB | diff a b | all | 1 |  |  |  |  |  | 0 B |  | oom-kill |
| 031 | generated | sql | 1.0 GiB | git diff --no-index a b | all | 1 | 42.43 | 42.19 | 0.22 | 5.3 MiB |  | 241 B | 1 | success |
| 032 | generated | sql | 1.0 GiB | diff a b | all | 2 |  |  |  |  |  |  |  | skipped |
| 033 | generated | sql | 1.0 GiB | git diff --no-index a b | all | 2 | 42.59 | 42.30 | 0.27 | 5.2 MiB |  | 241 B | 1 | success |
| 034 | generated | sql | 1.0 GiB | diff a b | all | 3 |  |  |  |  |  |  |  | skipped |
| 035 | generated | sql | 1.0 GiB | git diff --no-index a b | all | 3 | 44.41 | 44.16 | 0.24 | 5.4 MiB |  | 241 B | 1 | success |
| 036 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 1 | 1 | 1.09 | 0.59 | 0.48 | 95.2 MiB | 75.9 MiB | 673 B | 1 | success |
| 037 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 1 | 2 | 1.08 | 0.58 | 0.49 | 95.2 MiB | 75.9 MiB | 673 B | 1 | success |
| 038 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 1 | 3 | 1.08 | 0.59 | 0.49 | 95.2 MiB | 75.9 MiB | 673 B | 1 | success |
| 039 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 2 | 1 | 0.82 | 0.65 | 0.62 | 111.5 MiB | 82.5 MiB | 673 B | 1 | success |
| 040 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 2 | 2 | 0.72 | 0.67 | 0.63 | 111.6 MiB | 82.5 MiB | 673 B | 1 | success |
| 041 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 2 | 3 | 0.71 | 0.67 | 0.63 | 111.5 MiB | 82.5 MiB | 673 B | 1 | success |
| 042 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 4 | 1 | 0.53 | 0.73 | 1.00 | 112.0 MiB | 82.8 MiB | 673 B | 1 | success |
| 043 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 4 | 2 | 0.53 | 0.74 | 0.99 | 112.0 MiB | 82.8 MiB | 673 B | 1 | success |
| 044 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | 4 | 3 | 0.53 | 0.77 | 0.95 | 111.9 MiB | 82.8 MiB | 673 B | 1 | success |
| 045 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | all | 1 | 0.51 | 1.57 | 1.99 | 112.8 MiB | 83.6 MiB | 673 B | 1 | success |
| 046 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | all | 2 | 0.51 | 1.59 | 1.96 | 113.0 MiB | 83.6 MiB | 673 B | 1 | success |
| 047 | scaling | sql | 1.0 GiB | rift --summary --mem 256M | all | 3 | 0.51 | 1.61 | 1.96 | 113.2 MiB | 83.6 MiB | 673 B | 1 | success |
| 048 | generated | csv | 1.0 GiB | cat a b | all | 1 | 0.05 | 0.00 | 0.05 | 7.5 MiB |  |  | 0 | success |
| 049 | generated | csv | 1.0 GiB | cat a b | all | 2 | 0.05 | 0.00 | 0.05 | 7.6 MiB |  |  | 0 | success |
| 050 | generated | csv | 1.0 GiB | cat a b | all | 3 | 0.05 | 0.00 | 0.05 | 7.5 MiB |  |  | 0 | success |
| 051 | generated | csv | 1.0 GiB | rift --summary --mem 256M | all | 1 | 0.51 | 1.69 | 1.94 | 111.0 MiB | 83.0 MiB | 681 B | 1 | success |
| 052 | generated | csv | 1.0 GiB | rift --summary --mem 256M | all | 2 | 0.51 | 1.68 | 1.96 | 111.2 MiB | 83.0 MiB | 681 B | 1 | success |
| 053 | generated | csv | 1.0 GiB | rift --summary --mem 256M | all | 3 | 0.51 | 1.66 | 1.98 | 110.9 MiB | 83.0 MiB | 681 B | 1 | success |
| 054 | generated | csv | 1.0 GiB | rift --stats --mem 256M | all | 1 | 0.68 | 2.02 | 2.53 | 111.2 MiB | 83.0 MiB | 23.5 MiB | 1 | success |
| 055 | generated | csv | 1.0 GiB | rift --stats --mem 256M | all | 2 | 0.68 | 2.01 | 2.55 | 111.1 MiB | 83.0 MiB | 23.5 MiB | 1 | success |
| 056 | generated | csv | 1.0 GiB | rift --stats --mem 256M | all | 3 | 0.69 | 2.09 | 2.46 | 111.0 MiB | 83.0 MiB | 23.5 MiB | 1 | success |
| 057 | generated | csv | 1.0 GiB | rift --stats --mem 64M | all | 1 | 0.52 | 0.75 | 1.08 | 30.1 MiB | 20.8 MiB | 23.6 MiB | 1 | success |
| 058 | generated | csv | 1.0 GiB | rift --stats --mem 1G | all | 1 | 1.18 | 3.04 | 3.83 | 429.7 MiB | 327.2 MiB | 23.4 MiB | 1 | success |
| 059 | generated | csv | 1.0 GiB | diff a b | all | 1 |  |  |  |  |  | 0 B |  | oom-kill |
| 060 | generated | csv | 1.0 GiB | git diff --no-index a b | all | 1 | 96.51 | 96.29 | 0.21 | 5.4 MiB |  | 241 B | 1 | success |
| 061 | generated | csv | 1.0 GiB | diff a b | all | 2 |  |  |  |  |  |  |  | skipped |
| 062 | generated | csv | 1.0 GiB | git diff --no-index a b | all | 2 | 94.42 | 94.20 | 0.21 | 5.3 MiB |  | 241 B | 1 | success |
| 063 | generated | csv | 1.0 GiB | diff a b | all | 3 |  |  |  |  |  |  |  | skipped |
| 064 | generated | csv | 1.0 GiB | git diff --no-index a b | all | 3 | 98.95 | 98.74 | 0.20 | 5.2 MiB |  | 241 B | 1 | success |
| 065 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 1 | 1 | 1.06 | 0.57 | 0.49 | 93.2 MiB | 74.1 MiB | 681 B | 1 | success |
| 066 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 1 | 2 | 1.06 | 0.57 | 0.48 | 93.3 MiB | 74.1 MiB | 681 B | 1 | success |
| 067 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 1 | 3 | 1.06 | 0.57 | 0.48 | 93.2 MiB | 74.1 MiB | 681 B | 1 | success |
| 068 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 2 | 1 | 0.7 | 0.66 | 0.61 | 109.7 MiB | 81.9 MiB | 681 B | 1 | success |
| 069 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 2 | 2 | 0.69 | 0.66 | 0.58 | 109.6 MiB | 81.9 MiB | 681 B | 1 | success |
| 070 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 2 | 3 | 0.7 | 0.65 | 0.62 | 109.7 MiB | 81.9 MiB | 681 B | 1 | success |
| 071 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 4 | 1 | 0.52 | 0.75 | 0.94 | 110.1 MiB | 82.3 MiB | 681 B | 1 | success |
| 072 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 4 | 2 | 0.52 | 0.77 | 0.93 | 109.8 MiB | 82.3 MiB | 681 B | 1 | success |
| 073 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | 4 | 3 | 0.52 | 0.77 | 0.93 | 109.9 MiB | 82.3 MiB | 681 B | 1 | success |
| 074 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | all | 1 | 0.51 | 1.66 | 1.97 | 110.9 MiB | 83.0 MiB | 681 B | 1 | success |
| 075 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | all | 2 | 0.51 | 1.70 | 1.94 | 111.1 MiB | 83.0 MiB | 681 B | 1 | success |
| 076 | scaling | csv | 1.0 GiB | rift --summary --mem 256M | all | 3 | 0.51 | 1.69 | 1.95 | 111.2 MiB | 83.0 MiB | 681 B | 1 | success |
| 077 | generated | sql | 2.0 GiB | cat a b | all | 1 | 0.09 | 0.00 | 0.09 | 7.5 MiB |  |  | 0 | success |
| 078 | generated | sql | 2.0 GiB | cat a b | all | 2 | 0.1 | 0.00 | 0.10 | 7.5 MiB |  |  | 0 | success |
| 079 | generated | sql | 2.0 GiB | cat a b | all | 3 | 0.1 | 0.00 | 0.10 | 7.6 MiB |  |  | 0 | success |
| 080 | generated | sql | 2.0 GiB | rift --summary --mem 256M | all | 1 | 0.85 | 2.96 | 3.75 | 112.9 MiB | 83.6 MiB | 676 B | 1 | success |
| 081 | generated | sql | 2.0 GiB | rift --summary --mem 256M | all | 2 | 0.86 | 2.93 | 3.84 | 113.0 MiB | 83.6 MiB | 676 B | 1 | success |
| 082 | generated | sql | 2.0 GiB | rift --summary --mem 256M | all | 3 | 0.86 | 2.91 | 3.83 | 113.2 MiB | 83.6 MiB | 676 B | 1 | success |
| 083 | generated | sql | 2.0 GiB | rift --stats --mem 256M | all | 1 | 1.21 | 3.69 | 4.89 | 113.0 MiB | 83.6 MiB | 45.6 MiB | 1 | success |
| 084 | generated | sql | 2.0 GiB | rift --stats --mem 256M | all | 2 | 1.2 | 3.65 | 4.96 | 113.6 MiB | 83.6 MiB | 45.6 MiB | 1 | success |
| 085 | generated | sql | 2.0 GiB | rift --stats --mem 256M | all | 3 | 1.2 | 3.68 | 4.93 | 113.4 MiB | 83.6 MiB | 45.6 MiB | 1 | success |
| 086 | generated | sql | 2.0 GiB | rift --stats --mem 64M | all | 1 | 1.02 | 1.70 | 2.10 | 30.5 MiB | 21.0 MiB | 46.5 MiB | 1 | success |
| 087 | generated | sql | 2.0 GiB | rift --stats --mem 1G | all | 1 | 1.75 | 5.29 | 6.95 | 436.8 MiB | 330.0 MiB | 45.6 MiB | 1 | success |
| 088 | generated | csv | 2.0 GiB | cat a b | all | 1 | 0.09 | 0.00 | 0.09 | 7.5 MiB |  |  | 0 | success |
| 089 | generated | csv | 2.0 GiB | cat a b | all | 2 | 0.09 | 0.00 | 0.09 | 7.5 MiB |  |  | 0 | success |
| 090 | generated | csv | 2.0 GiB | cat a b | all | 3 | 0.09 | 0.00 | 0.09 | 7.5 MiB |  |  | 0 | success |
| 091 | generated | csv | 2.0 GiB | rift --summary --mem 256M | all | 1 | 0.86 | 3.10 | 3.76 | 111.2 MiB | 83.0 MiB | 686 B | 1 | success |
| 092 | generated | csv | 2.0 GiB | rift --summary --mem 256M | all | 2 | 0.86 | 3.03 | 3.83 | 111.2 MiB | 83.0 MiB | 686 B | 1 | success |
| 093 | generated | csv | 2.0 GiB | rift --summary --mem 256M | all | 3 | 0.86 | 3.07 | 3.81 | 111.0 MiB | 83.0 MiB | 686 B | 1 | success |
| 094 | generated | csv | 2.0 GiB | rift --stats --mem 256M | all | 1 | 1.2 | 3.84 | 4.82 | 111.0 MiB | 83.0 MiB | 46.6 MiB | 1 | success |
| 095 | generated | csv | 2.0 GiB | rift --stats --mem 256M | all | 2 | 1.2 | 3.80 | 4.91 | 111.1 MiB | 83.0 MiB | 46.6 MiB | 1 | success |
| 096 | generated | csv | 2.0 GiB | rift --stats --mem 256M | all | 3 | 1.2 | 3.77 | 4.90 | 110.9 MiB | 83.0 MiB | 46.6 MiB | 1 | success |
| 097 | generated | csv | 2.0 GiB | rift --stats --mem 64M | all | 1 | 0.96 | 1.59 | 2.09 | 30.0 MiB | 20.8 MiB | 47.0 MiB | 1 | success |
| 098 | generated | csv | 2.0 GiB | rift --stats --mem 1G | all | 1 | 1.73 | 5.45 | 6.90 | 430.9 MiB | 328.1 MiB | 46.5 MiB | 1 | success |
