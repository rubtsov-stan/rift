Files sit in tmpfs (RAM): this run measures CPU, memory and the algorithm, not the SSD.

Commands and notes write the run's scratch directory in /dev/shm as `$SCRATCH`.

## Host

- tool: bench/linux-safe.sh schema 4
- host: linux-intel-core-i5-10600-2026-11-18
- date: 2026-11-18T12:28:18Z
- cpu_model: Intel(R) Core(TM) i5-10600 CPU @ 3.30GHz
- cpu_threads: 12
- cpu_cores_per_socket: 6
- cpu_sockets: 1
- cpu_governor: powersave
- mem_total_bytes: 16111542272
- mem_available_bytes: 14224011264
- kernel: 7.0.0-43-generic
- distro: Ubuntu 26.04.1 LTS
- virt: none
- unknown: 
- rustc: rustc 1.99.1 (58cf8f9c3 2026-10-13)
- suite: tools
- reps: 5
- rift_ref: v0.3.4
- rift_commit: 0608c57d55402ec2be2c275b79c9c271d228ca60
- scratch_fs: tmpfs, 7.5 GiB free of the mount at start
- limiter: systemd
- load_before: 1.79 0.59 0.23
- version_diff: diff (GNU diffutils) 3.12
- version_git: git version 2.53.0
- version_difft: Difftastic 0.72.0
- version_duckdb: v2.0.0 (Cyanoptera) d54cbbe3df
- version_csvdiff: csvdiff version 1.4.0
- version_qsv: qsv 29.0.0-jemalloc+bgthread--12-12;12.00
- version_daff: daff 1.4.2
- version_sort: sort (uutils coreutils) 0.10.0
- version_cmp: cmp (GNU diffutils) 3.12
- tool_timeout_s: 600
- load_after: 4.94 2.37 1.49

## Plan

```
Plan
  MemAvailable      13.2 GiB of 15.0 GiB; 60% budget 7.9 GiB
  scratch           tmpfs /dev/shm (7.5 GiB free)
  source            https://github.com/rubtsov-stan/rift at v0.3.4
  suite             tools
  repetitions       5 of each repeated run
  memory cap        systemd-run --user scope, MemoryMax, MemorySwapMax=0: per pair size, below
  pairs             seed 7, 0.1% changes; profiles sql and csv for the line tools, shuffled csv for the key tools
    256 MiB           in tmpfs, cap 5.4 GiB
    1.0 GiB           in tmpfs, cap 3.9 GiB
    4.0 GiB           not run: the pair and a 2 GiB cap do not fit within 60% of MemAvailable; --disk-dir puts it on a disk
  line tools        rift --stats --mem 256M, diff, git diff --no-index, git diff --no-index --text, difft, cat a b | wc -c, cmp, sort | comm -3
  key tools         rift --key csv:id, DuckDB (FULL OUTER JOIN and EXCEPT, each at its default memory_limit and at 256MB), csvdiff, qsv diff, daff (256 MiB only)
  runs              5 of each; any run is stopped after 600 s; a command that is killed, stopped or prints no counts skips its later runs and larger sizes
  RSS timeline      every 100 ms on the 1 GiB pairs: rift, diff and difft on sql, rift --key and DuckDB on the key pair
  diff             diff (GNU diffutils) 3.12
  git              git version 2.53.0
  difft            Difftastic 0.72.0
  duckdb           v2.0.0 (Cyanoptera) d54cbbe3df
  csvdiff          csvdiff version 1.4.0
  qsv              qsv 29.0.0-jemalloc+bgthread--12-12;12.00
  daff             daff 1.4.2
  output            rift-bench-linux-intel-core-i5-10600-2026-11-18.tar.gz in the current directory
```

## Notes

- duckdb full outer join, memory_limit 256MB on keys 1.0 GiB not run: it failed on a smaller pair
- duckdb except, memory_limit 256MB on keys 1.0 GiB not run: it failed on a smaller pair
- tools: 4.0 GiB pairs not run: the pair and a 2 GiB cap do not fit within 60% of MemAvailable (--disk-dir puts them on a disk)
- tools: diff and difft run on one thread; rift, DuckDB and qsv use every CPU. difftastic is built for code: above its size limits it falls back to a line diff, which its output names "Text". git diff --no-index calls files over 512 MiB binary and prints no diff unless given --text. cmp stops at the first differing byte, so its time is not a read of both files. sort | comm compares sorted lines as sets, so moved lines do not count as changes. DuckDB spills to $SCRATCH/duck.tmp, which is tmpfs, so a spill is RAM charged to its cap.

## Runs

wall, user and sys in seconds; peak heap is rift's own count; result is systemd's verdict on the run's scope (oom-kill means the cap killed it; skipped means an earlier repetition was killed, so this one was not run; n/a without systemd).

| id | group | profile | size | tool | threads | rep | wall | user | sys | max RSS | peak heap | output | exit | result |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| 001 | tools | sql | 256.0 MiB | cat a b | wc -c | all | 1 | 0.01 | 0.00 | 0.01 | 7.9 MiB |  | 512.2 MiB | 0 | success |
| 002 | tools | sql | 256.0 MiB | cat a b | wc -c | all | 2 | 0.01 | 0.00 | 0.01 | 7.7 MiB |  | 512.2 MiB | 0 | success |
| 003 | tools | sql | 256.0 MiB | cat a b | wc -c | all | 3 | 0.01 | 0.00 | 0.01 | 7.8 MiB |  | 512.2 MiB | 0 | success |
| 004 | tools | sql | 256.0 MiB | cat a b | wc -c | all | 4 | 0.01 | 0.00 | 0.01 | 7.8 MiB |  | 512.2 MiB | 0 | success |
| 005 | tools | sql | 256.0 MiB | cat a b | wc -c | all | 5 | 0.01 | 0.00 | 0.01 | 7.9 MiB |  | 512.2 MiB | 0 | success |
| 006 | tools | sql | 256.0 MiB | cmp a b | all | 1 | 0 | 0.00 | 0.00 | 7.8 MiB |  | 95 B | 1 | success |
| 007 | tools | sql | 256.0 MiB | cmp a b | all | 2 | 0 | 0.00 | 0.00 | 7.8 MiB |  | 95 B | 1 | success |
| 008 | tools | sql | 256.0 MiB | cmp a b | all | 3 | 0 | 0.00 | 0.00 | 7.7 MiB |  | 95 B | 1 | success |
| 009 | tools | sql | 256.0 MiB | cmp a b | all | 4 | 0 | 0.00 | 0.00 | 7.8 MiB |  | 95 B | 1 | success |
| 010 | tools | sql | 256.0 MiB | cmp a b | all | 5 | 0 | 0.00 | 0.00 | 7.5 MiB |  | 95 B | 1 | success |
| 011 | tools | sql | 256.0 MiB | rift --stats --mem 256M | all | 1 | 0.22 | 0.59 | 0.45 | 107.9 MiB | 79.0 MiB | 5.9 MiB | 1 | success |
| 012 | tools | sql | 256.0 MiB | rift --stats --mem 256M | all | 2 | 0.22 | 0.61 | 0.43 | 108.4 MiB | 79.0 MiB | 5.9 MiB | 1 | success |
| 013 | tools | sql | 256.0 MiB | rift --stats --mem 256M | all | 3 | 0.22 | 0.60 | 0.43 | 108.4 MiB | 79.0 MiB | 5.9 MiB | 1 | success |
| 014 | tools | sql | 256.0 MiB | rift --stats --mem 256M | all | 4 | 0.22 | 0.60 | 0.44 | 108.3 MiB | 79.0 MiB | 5.9 MiB | 1 | success |
| 015 | tools | sql | 256.0 MiB | rift --stats --mem 256M | all | 5 | 0.22 | 0.61 | 0.42 | 108.7 MiB | 79.0 MiB | 5.9 MiB | 1 | success |
| 016 | tools | sql | 256.0 MiB | diff a b | all | 1 | 1.61 | 1.30 | 0.31 | 776.2 MiB |  | 5.2 MiB | 1 | success |
| 017 | tools | sql | 256.0 MiB | diff a b | all | 2 | 1.62 | 1.30 | 0.31 | 776.2 MiB |  | 5.2 MiB | 1 | success |
| 018 | tools | sql | 256.0 MiB | diff a b | all | 3 | 1.63 | 1.33 | 0.29 | 776.2 MiB |  | 5.2 MiB | 1 | success |
| 019 | tools | sql | 256.0 MiB | diff a b | all | 4 | 1.61 | 1.31 | 0.30 | 776.2 MiB |  | 5.2 MiB | 1 | success |
| 020 | tools | sql | 256.0 MiB | diff a b | all | 5 | 1.61 | 1.31 | 0.30 | 776.2 MiB |  | 5.2 MiB | 1 | success |
| 021 | tools | sql | 256.0 MiB | git diff --no-index a b | all | 1 | 2.58 | 2.36 | 0.21 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 022 | tools | sql | 256.0 MiB | git diff --no-index a b | all | 2 | 2.59 | 2.39 | 0.20 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 023 | tools | sql | 256.0 MiB | git diff --no-index a b | all | 3 | 2.58 | 2.39 | 0.18 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 024 | tools | sql | 256.0 MiB | git diff --no-index a b | all | 4 | 2.58 | 2.37 | 0.20 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 025 | tools | sql | 256.0 MiB | git diff --no-index a b | all | 5 | 2.6 | 2.38 | 0.21 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 026 | tools | sql | 256.0 MiB | git diff --no-index --text a b | all | 1 | 2.59 | 2.37 | 0.20 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 027 | tools | sql | 256.0 MiB | git diff --no-index --text a b | all | 2 | 2.6 | 2.41 | 0.18 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 028 | tools | sql | 256.0 MiB | git diff --no-index --text a b | all | 3 | 2.59 | 2.38 | 0.19 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 029 | tools | sql | 256.0 MiB | git diff --no-index --text a b | all | 4 | 2.59 | 2.37 | 0.20 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 030 | tools | sql | 256.0 MiB | git diff --no-index --text a b | all | 5 | 2.58 | 2.38 | 0.19 | 1.0 GiB |  | 6.0 MiB | 1 | success |
| 031 | tools | sql | 256.0 MiB | difft a b | all | 1 | 11.8 | 9.59 | 2.20 | 3.7 GiB |  | 9.4 MiB | 0 | success |
| 032 | tools | sql | 256.0 MiB | difft a b | all | 2 | 11.77 | 9.48 | 2.29 | 3.6 GiB |  | 9.4 MiB | 0 | success |
| 033 | tools | sql | 256.0 MiB | difft a b | all | 3 | 11.81 | 9.50 | 2.30 | 3.6 GiB |  | 9.4 MiB | 0 | success |
| 034 | tools | sql | 256.0 MiB | difft a b | all | 4 | 11.76 | 9.49 | 2.27 | 3.6 GiB |  | 9.4 MiB | 0 | success |
| 035 | tools | sql | 256.0 MiB | difft a b | all | 5 | 11.87 | 9.62 | 2.25 | 3.6 GiB |  | 9.4 MiB | 0 | success |
| 036 | tools | sql | 256.0 MiB | sort a, sort b, comm -3 | all | 1 | 1.97 | 5.97 | 0.46 | 340.9 MiB |  | 3.5 MiB | 0 | success |
| 037 | tools | sql | 256.0 MiB | sort a, sort b, comm -3 | all | 2 | 1.97 | 6.00 | 0.44 | 340.9 MiB |  | 3.5 MiB | 0 | success |
| 038 | tools | sql | 256.0 MiB | sort a, sort b, comm -3 | all | 3 | 1.97 | 6.02 | 0.44 | 340.9 MiB |  | 3.5 MiB | 0 | success |
| 039 | tools | sql | 256.0 MiB | sort a, sort b, comm -3 | all | 4 | 1.97 | 6.03 | 0.44 | 340.9 MiB |  | 3.5 MiB | 0 | success |
| 040 | tools | sql | 256.0 MiB | sort a, sort b, comm -3 | all | 5 | 1.97 | 5.99 | 0.46 | 341.0 MiB |  | 3.5 MiB | 0 | success |
| 041 | tools | csv | 256.0 MiB | cat a b | wc -c | all | 1 | 0.02 | 0.00 | 0.01 | 7.8 MiB |  | 512.0 MiB | 0 | success |
| 042 | tools | csv | 256.0 MiB | cat a b | wc -c | all | 2 | 0.01 | 0.00 | 0.01 | 7.8 MiB |  | 512.0 MiB | 0 | success |
| 043 | tools | csv | 256.0 MiB | cat a b | wc -c | all | 3 | 0.01 | 0.00 | 0.01 | 7.8 MiB |  | 512.0 MiB | 0 | success |
| 044 | tools | csv | 256.0 MiB | cat a b | wc -c | all | 4 | 0.01 | 0.00 | 0.01 | 7.7 MiB |  | 512.0 MiB | 0 | success |
| 045 | tools | csv | 256.0 MiB | cat a b | wc -c | all | 5 | 0.01 | 0.00 | 0.01 | 7.8 MiB |  | 512.0 MiB | 0 | success |
| 046 | tools | csv | 256.0 MiB | cmp a b | all | 1 | 0 | 0.00 | 0.00 | 7.8 MiB |  | 97 B | 1 | success |
| 047 | tools | csv | 256.0 MiB | cmp a b | all | 2 | 0 | 0.00 | 0.00 | 7.9 MiB |  | 97 B | 1 | success |
| 048 | tools | csv | 256.0 MiB | cmp a b | all | 3 | 0 | 0.00 | 0.00 | 7.9 MiB |  | 97 B | 1 | success |
| 049 | tools | csv | 256.0 MiB | cmp a b | all | 4 | 0 | 0.00 | 0.00 | 8.0 MiB |  | 97 B | 1 | success |
| 050 | tools | csv | 256.0 MiB | cmp a b | all | 5 | 0 | 0.00 | 0.00 | 7.7 MiB |  | 97 B | 1 | success |
| 051 | tools | csv | 256.0 MiB | rift --stats --mem 256M | all | 1 | 0.22 | 0.63 | 0.42 | 106.2 MiB | 78.2 MiB | 5.4 MiB | 1 | success |
| 052 | tools | csv | 256.0 MiB | rift --stats --mem 256M | all | 2 | 0.22 | 0.61 | 0.44 | 106.4 MiB | 78.2 MiB | 5.4 MiB | 1 | success |
| 053 | tools | csv | 256.0 MiB | rift --stats --mem 256M | all | 3 | 0.22 | 0.60 | 0.44 | 106.3 MiB | 78.2 MiB | 5.4 MiB | 1 | success |
| 054 | tools | csv | 256.0 MiB | rift --stats --mem 256M | all | 4 | 0.22 | 0.60 | 0.43 | 106.1 MiB | 78.2 MiB | 5.4 MiB | 1 | success |
| 055 | tools | csv | 256.0 MiB | rift --stats --mem 256M | all | 5 | 0.22 | 0.59 | 0.45 | 106.3 MiB | 78.2 MiB | 5.4 MiB | 1 | success |
| 056 | tools | csv | 256.0 MiB | diff a b | all | 1 | 1.52 | 1.21 | 0.31 | 764.9 MiB |  | 4.7 MiB | 1 | success |
| 057 | tools | csv | 256.0 MiB | diff a b | all | 2 | 1.54 | 1.25 | 0.29 | 764.9 MiB |  | 4.7 MiB | 1 | success |
| 058 | tools | csv | 256.0 MiB | diff a b | all | 3 | 1.55 | 1.23 | 0.31 | 764.9 MiB |  | 4.7 MiB | 1 | success |
| 059 | tools | csv | 256.0 MiB | diff a b | all | 4 | 1.54 | 1.23 | 0.30 | 764.9 MiB |  | 4.7 MiB | 1 | success |
| 060 | tools | csv | 256.0 MiB | diff a b | all | 5 | 1.56 | 1.24 | 0.31 | 764.9 MiB |  | 4.7 MiB | 1 | success |
| 061 | tools | csv | 256.0 MiB | git diff --no-index a b | all | 1 | 2.52 | 2.33 | 0.19 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 062 | tools | csv | 256.0 MiB | git diff --no-index a b | all | 2 | 2.53 | 2.29 | 0.22 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 063 | tools | csv | 256.0 MiB | git diff --no-index a b | all | 3 | 2.54 | 2.34 | 0.19 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 064 | tools | csv | 256.0 MiB | git diff --no-index a b | all | 4 | 2.52 | 2.31 | 0.20 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 065 | tools | csv | 256.0 MiB | git diff --no-index a b | all | 5 | 2.54 | 2.34 | 0.19 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 066 | tools | csv | 256.0 MiB | git diff --no-index --text a b | all | 1 | 2.54 | 2.34 | 0.20 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 067 | tools | csv | 256.0 MiB | git diff --no-index --text a b | all | 2 | 2.54 | 2.32 | 0.21 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 068 | tools | csv | 256.0 MiB | git diff --no-index --text a b | all | 3 | 2.54 | 2.33 | 0.21 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 069 | tools | csv | 256.0 MiB | git diff --no-index --text a b | all | 4 | 2.54 | 2.34 | 0.19 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 070 | tools | csv | 256.0 MiB | git diff --no-index --text a b | all | 5 | 2.52 | 2.32 | 0.19 | 1.0 GiB |  | 5.5 MiB | 1 | success |
| 071 | tools | csv | 256.0 MiB | difft a b | all | 1 | 10.5 | 8.32 | 2.17 | 3.6 GiB |  | 6.5 MiB | 0 | success |
| 072 | tools | csv | 256.0 MiB | difft a b | all | 2 | 10.49 | 8.36 | 2.12 | 3.6 GiB |  | 6.5 MiB | 0 | success |
| 073 | tools | csv | 256.0 MiB | difft a b | all | 3 | 10.56 | 8.42 | 2.14 | 3.6 GiB |  | 6.5 MiB | 0 | success |
| 074 | tools | csv | 256.0 MiB | difft a b | all | 4 | 10.59 | 8.37 | 2.21 | 3.6 GiB |  | 6.5 MiB | 0 | success |
| 075 | tools | csv | 256.0 MiB | difft a b | all | 5 | 10.57 | 8.41 | 2.15 | 3.6 GiB |  | 6.5 MiB | 0 | success |
| 076 | tools | csv | 256.0 MiB | sort a, sort b, comm -3 | all | 1 | 1.7 | 3.91 | 0.46 | 337.8 MiB |  | 3.2 MiB | 0 | success |
| 077 | tools | csv | 256.0 MiB | sort a, sort b, comm -3 | all | 2 | 1.69 | 3.91 | 0.46 | 337.6 MiB |  | 3.2 MiB | 0 | success |
| 078 | tools | csv | 256.0 MiB | sort a, sort b, comm -3 | all | 3 | 1.69 | 3.85 | 0.48 | 337.4 MiB |  | 3.2 MiB | 0 | success |
| 079 | tools | csv | 256.0 MiB | sort a, sort b, comm -3 | all | 4 | 1.69 | 3.83 | 0.44 | 337.4 MiB |  | 3.2 MiB | 0 | success |
| 080 | tools | csv | 256.0 MiB | sort a, sort b, comm -3 | all | 5 | 1.69 | 3.87 | 0.46 | 337.2 MiB |  | 3.2 MiB | 0 | success |
| 081 | tools | keys | 256.0 MiB | rift --key csv:id | all | 1 | 0.36 | 1.91 | 0.14 | 72.7 MiB |  | 1.9 MiB | 1 | success |
| 082 | tools | keys | 256.0 MiB | rift --key csv:id | all | 2 | 0.36 | 1.93 | 0.12 | 72.6 MiB |  | 1.9 MiB | 1 | success |
| 083 | tools | keys | 256.0 MiB | rift --key csv:id | all | 3 | 0.36 | 1.89 | 0.15 | 72.7 MiB |  | 1.9 MiB | 1 | success |
| 084 | tools | keys | 256.0 MiB | rift --key csv:id | all | 4 | 0.36 | 1.91 | 0.13 | 72.7 MiB |  | 1.9 MiB | 1 | success |
| 085 | tools | keys | 256.0 MiB | rift --key csv:id | all | 5 | 0.36 | 1.91 | 0.14 | 72.8 MiB |  | 1.9 MiB | 1 | success |
| 086 | tools | keys | 256.0 MiB | duckdb full outer join | all | 1 | 0.73 | 5.58 | 0.73 | 1.2 GiB |  | 61 B | 0 | success |
| 087 | tools | keys | 256.0 MiB | duckdb full outer join | all | 2 | 0.73 | 5.55 | 0.68 | 1.1 GiB |  | 61 B | 0 | success |
| 088 | tools | keys | 256.0 MiB | duckdb full outer join | all | 3 | 0.73 | 5.57 | 0.68 | 1.2 GiB |  | 61 B | 0 | success |
| 089 | tools | keys | 256.0 MiB | duckdb full outer join | all | 4 | 0.73 | 5.58 | 0.73 | 1.1 GiB |  | 61 B | 0 | success |
| 090 | tools | keys | 256.0 MiB | duckdb full outer join | all | 5 | 0.73 | 5.48 | 0.76 | 1.2 GiB |  | 61 B | 0 | success |
| 091 | tools | keys | 256.0 MiB | duckdb full outer join, memory_limit 256MB | all | 1 | 0.21 | 0.67 | 0.20 | 260.3 MiB |  | 35 B | 1 | success |
| 092 | tools | keys | 256.0 MiB | duckdb full outer join, memory_limit 256MB | all | 2 | 0.21 | 0.73 | 0.17 | 275.1 MiB |  | 35 B | 1 | success |
| 093 | tools | keys | 256.0 MiB | duckdb full outer join, memory_limit 256MB | all | 3 | 0.21 | 0.76 | 0.18 | 275.9 MiB |  | 35 B | 1 | success |
| 094 | tools | keys | 256.0 MiB | duckdb full outer join, memory_limit 256MB | all | 4 | 0.21 | 0.78 | 0.17 | 273.9 MiB |  | 35 B | 1 | success |
| 095 | tools | keys | 256.0 MiB | duckdb full outer join, memory_limit 256MB | all | 5 | 0.22 | 0.76 | 0.19 | 272.4 MiB |  | 35 B | 1 | success |
| 096 | tools | keys | 256.0 MiB | duckdb except | all | 1 | 1.32 | 10.18 | 1.25 | 1.6 GiB |  | 47 B | 0 | success |
| 097 | tools | keys | 256.0 MiB | duckdb except | all | 2 | 1.33 | 10.18 | 1.18 | 1.6 GiB |  | 47 B | 0 | success |
| 098 | tools | keys | 256.0 MiB | duckdb except | all | 3 | 1.34 | 10.30 | 1.16 | 1.5 GiB |  | 47 B | 0 | success |
| 099 | tools | keys | 256.0 MiB | duckdb except | all | 4 | 1.32 | 10.21 | 1.13 | 1.5 GiB |  | 47 B | 0 | success |
| 100 | tools | keys | 256.0 MiB | duckdb except | all | 5 | 1.33 | 10.26 | 1.20 | 1.6 GiB |  | 47 B | 0 | success |
| 101 | tools | keys | 256.0 MiB | duckdb except, memory_limit 256MB | all | 1 | 0.3 | 0.70 | 0.20 | 263.1 MiB |  | 27 B | 1 | success |
| 102 | tools | keys | 256.0 MiB | duckdb except, memory_limit 256MB | all | 2 | 0.31 | 0.88 | 0.21 | 269.2 MiB |  | 27 B | 1 | success |
| 103 | tools | keys | 256.0 MiB | duckdb except, memory_limit 256MB | all | 3 | 0.3 | 0.81 | 0.22 | 271.7 MiB |  | 27 B | 1 | success |
| 104 | tools | keys | 256.0 MiB | duckdb except, memory_limit 256MB | all | 4 | 0.31 | 0.80 | 0.21 | 268.2 MiB |  | 27 B | 1 | success |
| 105 | tools | keys | 256.0 MiB | duckdb except, memory_limit 256MB | all | 5 | 0.27 | 0.46 | 0.18 | 252.6 MiB |  | 27 B | 1 | success |
| 106 | tools | keys | 256.0 MiB | csvdiff -p 0 | all | 1 | 3.23 | 11.77 | 0.76 | 2.0 GiB |  | 10.3 MiB | 0 | success |
| 107 | tools | keys | 256.0 MiB | csvdiff -p 0 | all | 2 | 3.17 | 12.08 | 0.72 | 1.8 GiB |  | 10.3 MiB | 0 | success |
| 108 | tools | keys | 256.0 MiB | csvdiff -p 0 | all | 3 | 3.26 | 11.72 | 0.81 | 2.0 GiB |  | 10.3 MiB | 0 | success |
| 109 | tools | keys | 256.0 MiB | csvdiff -p 0 | all | 4 | 3.22 | 12.20 | 0.73 | 1.8 GiB |  | 10.3 MiB | 0 | success |
| 110 | tools | keys | 256.0 MiB | csvdiff -p 0 | all | 5 | 3.14 | 11.84 | 0.71 | 1.7 GiB |  | 10.3 MiB | 0 | success |
| 111 | tools | keys | 256.0 MiB | qsv diff -k 0 | all | 1 | 1.13 | 3.02 | 0.11 | 112.1 MiB |  | 10.3 MiB | 0 | success |
| 112 | tools | keys | 256.0 MiB | qsv diff -k 0 | all | 2 | 1.16 | 3.09 | 0.10 | 129.3 MiB |  | 10.3 MiB | 0 | success |
| 113 | tools | keys | 256.0 MiB | qsv diff -k 0 | all | 3 | 1.21 | 3.11 | 0.15 | 170.8 MiB |  | 10.3 MiB | 0 | success |
| 114 | tools | keys | 256.0 MiB | qsv diff -k 0 | all | 4 | 1.15 | 3.08 | 0.10 | 132.0 MiB |  | 10.3 MiB | 0 | success |
| 115 | tools | keys | 256.0 MiB | qsv diff -k 0 | all | 5 | 1.15 | 3.08 | 0.10 | 126.6 MiB |  | 10.3 MiB | 0 | success |
| 116 | tools | keys | 256.0 MiB | daff diff --id id | all | 1 |  |  |  |  |  | 0 B |  | oom-kill |
| 117 | tools | keys | 256.0 MiB | daff diff --id id | all | 2 |  |  |  |  |  |  |  | skipped |
| 118 | tools | keys | 256.0 MiB | daff diff --id id | all | 3 |  |  |  |  |  |  |  | skipped |
| 119 | tools | keys | 256.0 MiB | daff diff --id id | all | 4 |  |  |  |  |  |  |  | skipped |
| 120 | tools | keys | 256.0 MiB | daff diff --id id | all | 5 |  |  |  |  |  |  |  | skipped |
| 121 | tools | sql | 1.0 GiB | cat a b | wc -c | all | 1 | 0.05 | 0.00 | 0.05 | 7.8 MiB |  | 2.0 GiB | 0 | success |
| 122 | tools | sql | 1.0 GiB | cat a b | wc -c | all | 2 | 0.05 | 0.00 | 0.05 | 7.8 MiB |  | 2.0 GiB | 0 | success |
| 123 | tools | sql | 1.0 GiB | cat a b | wc -c | all | 3 | 0.05 | 0.00 | 0.05 | 7.8 MiB |  | 2.0 GiB | 0 | success |
| 124 | tools | sql | 1.0 GiB | cat a b | wc -c | all | 4 | 0.05 | 0.00 | 0.05 | 7.7 MiB |  | 2.0 GiB | 0 | success |
| 125 | tools | sql | 1.0 GiB | cat a b | wc -c | all | 5 | 0.05 | 0.00 | 0.05 | 7.6 MiB |  | 2.0 GiB | 0 | success |
| 126 | tools | sql | 1.0 GiB | cmp a b | all | 1 | 0 | 0.00 | 0.00 | 8.0 MiB |  | 95 B | 1 | success |
| 127 | tools | sql | 1.0 GiB | cmp a b | all | 2 | 0 | 0.00 | 0.00 | 7.8 MiB |  | 95 B | 1 | success |
| 128 | tools | sql | 1.0 GiB | cmp a b | all | 3 | 0 | 0.00 | 0.00 | 7.9 MiB |  | 95 B | 1 | success |
| 129 | tools | sql | 1.0 GiB | cmp a b | all | 4 | 0 | 0.00 | 0.00 | 7.7 MiB |  | 95 B | 1 | success |
| 130 | tools | sql | 1.0 GiB | cmp a b | all | 5 | 0 | 0.00 | 0.00 | 7.8 MiB |  | 95 B | 1 | success |
| 131 | tools | sql | 1.0 GiB | rift --stats --mem 256M | all | 1 | 0.55 | 1.42 | 1.59 | 109.1 MiB | 79.4 MiB | 23.0 MiB | 1 | success |
| 132 | tools | sql | 1.0 GiB | rift --stats --mem 256M | all | 2 | 0.55 | 1.41 | 1.60 | 109.5 MiB | 79.4 MiB | 23.0 MiB | 1 | success |
| 133 | tools | sql | 1.0 GiB | rift --stats --mem 256M | all | 3 | 0.55 | 1.42 | 1.58 | 109.4 MiB | 79.4 MiB | 23.0 MiB | 1 | success |
| 134 | tools | sql | 1.0 GiB | rift --stats --mem 256M | all | 4 | 0.55 | 1.47 | 1.53 | 109.5 MiB | 79.4 MiB | 23.0 MiB | 1 | success |
| 135 | tools | sql | 1.0 GiB | rift --stats --mem 256M | all | 5 | 0.55 | 1.44 | 1.57 | 109.6 MiB | 79.4 MiB | 23.0 MiB | 1 | success |
| 136 | tools | sql | 1.0 GiB | diff a b | all | 1 | 7.67 | 6.45 | 1.20 | 3.0 GiB |  | 20.1 MiB | 1 | success |
| 137 | tools | sql | 1.0 GiB | diff a b | all | 2 | 7.66 | 6.43 | 1.22 | 3.0 GiB |  | 20.1 MiB | 1 | success |
| 138 | tools | sql | 1.0 GiB | diff a b | all | 3 | 7.7 | 6.47 | 1.21 | 3.0 GiB |  | 20.1 MiB | 1 | success |
| 139 | tools | sql | 1.0 GiB | diff a b | all | 4 | 7.67 | 6.43 | 1.23 | 3.0 GiB |  | 20.1 MiB | 1 | success |
| 140 | tools | sql | 1.0 GiB | diff a b | all | 5 | 7.65 | 6.43 | 1.21 | 3.0 GiB |  | 20.1 MiB | 1 | success |
| 141 | tools | sql | 1.0 GiB | git diff --no-index a b | all | 1 | 42.57 | 42.36 | 0.21 | 7.7 MiB |  | 201 B | 1 | success |
| 142 | tools | sql | 1.0 GiB | git diff --no-index a b | all | 2 | 42.96 | 42.71 | 0.24 | 7.8 MiB |  | 201 B | 1 | success |
| 143 | tools | sql | 1.0 GiB | git diff --no-index a b | all | 3 | 42.51 | 42.28 | 0.23 | 7.8 MiB |  | 201 B | 1 | success |
| 144 | tools | sql | 1.0 GiB | git diff --no-index a b | all | 4 | 43.28 | 43.04 | 0.24 | 7.8 MiB |  | 201 B | 1 | success |
| 145 | tools | sql | 1.0 GiB | git diff --no-index a b | all | 5 | 42.58 | 42.37 | 0.20 | 7.8 MiB |  | 201 B | 1 | success |
| 146 | tools | sql | 1.0 GiB | git diff --no-index --text a b | all | 1 | 42.92 | 42.69 | 0.22 | 7.9 MiB |  | 0 B | 128 | success |
| 147 | tools | sql | 1.0 GiB | git diff --no-index --text a b | all | 2 | 42.5 | 42.28 | 0.21 | 7.9 MiB |  | 0 B | 128 | success |
| 148 | tools | sql | 1.0 GiB | git diff --no-index --text a b | all | 3 | 42.5 | 42.26 | 0.23 | 7.9 MiB |  | 0 B | 128 | success |
| 149 | tools | sql | 1.0 GiB | git diff --no-index --text a b | all | 4 | 42.69 | 42.47 | 0.22 | 7.9 MiB |  | 0 B | 128 | success |
| 150 | tools | sql | 1.0 GiB | git diff --no-index --text a b | all | 5 | 42.33 | 42.08 | 0.24 | 7.8 MiB |  | 0 B | 128 | success |
| 151 | tools | sql | 1.0 GiB | difft a b | all | 1 |  |  |  |  |  | 0 B |  | oom-kill |
| 152 | tools | sql | 1.0 GiB | difft a b | all | 2 |  |  |  |  |  |  |  | skipped |
| 153 | tools | sql | 1.0 GiB | difft a b | all | 3 |  |  |  |  |  |  |  | skipped |
| 154 | tools | sql | 1.0 GiB | difft a b | all | 4 |  |  |  |  |  |  |  | skipped |
| 155 | tools | sql | 1.0 GiB | difft a b | all | 5 |  |  |  |  |  |  |  | skipped |
| 156 | tools | sql | 1.0 GiB | sort a, sort b, comm -3 | all | 1 | 8.4 | 29.15 | 2.81 | 1.8 GiB |  | 13.5 MiB | 0 | success |
| 157 | tools | sql | 1.0 GiB | sort a, sort b, comm -3 | all | 2 | 8.45 | 29.09 | 2.76 | 1.8 GiB |  | 13.5 MiB | 0 | success |
| 158 | tools | sql | 1.0 GiB | sort a, sort b, comm -3 | all | 3 | 8.42 | 29.10 | 2.79 | 1.8 GiB |  | 13.5 MiB | 0 | success |
| 159 | tools | sql | 1.0 GiB | sort a, sort b, comm -3 | all | 4 | 8.41 | 29.07 | 2.72 | 1.8 GiB |  | 13.5 MiB | 0 | success |
| 160 | tools | sql | 1.0 GiB | sort a, sort b, comm -3 | all | 5 | 8.43 | 29.29 | 2.76 | 1.8 GiB |  | 13.5 MiB | 0 | success |
| 161 | tools | csv | 1.0 GiB | cat a b | wc -c | all | 1 | 0.05 | 0.00 | 0.05 | 7.7 MiB |  | 2.0 GiB | 0 | success |
| 162 | tools | csv | 1.0 GiB | cat a b | wc -c | all | 2 | 0.05 | 0.00 | 0.05 | 7.7 MiB |  | 2.0 GiB | 0 | success |
| 163 | tools | csv | 1.0 GiB | cat a b | wc -c | all | 3 | 0.05 | 0.00 | 0.05 | 7.8 MiB |  | 2.0 GiB | 0 | success |
| 164 | tools | csv | 1.0 GiB | cat a b | wc -c | all | 4 | 0.05 | 0.00 | 0.05 | 7.7 MiB |  | 2.0 GiB | 0 | success |
| 165 | tools | csv | 1.0 GiB | cat a b | wc -c | all | 5 | 0.05 | 0.00 | 0.05 | 7.8 MiB |  | 2.0 GiB | 0 | success |
| 166 | tools | csv | 1.0 GiB | cmp a b | all | 1 | 0 | 0.00 | 0.00 | 7.8 MiB |  | 97 B | 1 | success |
| 167 | tools | csv | 1.0 GiB | cmp a b | all | 2 | 0 | 0.00 | 0.00 | 7.9 MiB |  | 97 B | 1 | success |
| 168 | tools | csv | 1.0 GiB | cmp a b | all | 3 | 0 | 0.00 | 0.00 | 7.8 MiB |  | 97 B | 1 | success |
| 169 | tools | csv | 1.0 GiB | cmp a b | all | 4 | 0 | 0.00 | 0.00 | 7.9 MiB |  | 97 B | 1 | success |
| 170 | tools | csv | 1.0 GiB | cmp a b | all | 5 | 0 | 0.00 | 0.00 | 7.7 MiB |  | 97 B | 1 | success |
| 171 | tools | csv | 1.0 GiB | rift --stats --mem 256M | all | 1 | 0.56 | 1.49 | 1.55 | 107.6 MiB | 78.8 MiB | 23.5 MiB | 1 | success |
| 172 | tools | csv | 1.0 GiB | rift --stats --mem 256M | all | 2 | 0.55 | 1.42 | 1.61 | 108.1 MiB | 78.8 MiB | 23.5 MiB | 1 | success |
| 173 | tools | csv | 1.0 GiB | rift --stats --mem 256M | all | 3 | 0.55 | 1.41 | 1.63 | 107.7 MiB | 78.8 MiB | 23.5 MiB | 1 | success |
| 174 | tools | csv | 1.0 GiB | rift --stats --mem 256M | all | 4 | 0.55 | 1.45 | 1.59 | 107.8 MiB | 78.8 MiB | 23.5 MiB | 1 | success |
| 175 | tools | csv | 1.0 GiB | rift --stats --mem 256M | all | 5 | 0.55 | 1.47 | 1.55 | 107.8 MiB | 78.8 MiB | 23.5 MiB | 1 | success |
| 176 | tools | csv | 1.0 GiB | diff a b | all | 1 | 7.65 | 6.46 | 1.18 | 3.0 GiB |  | 20.6 MiB | 1 | success |
| 177 | tools | csv | 1.0 GiB | diff a b | all | 2 | 7.66 | 6.45 | 1.20 | 3.0 GiB |  | 20.6 MiB | 1 | success |
| 178 | tools | csv | 1.0 GiB | diff a b | all | 3 | 7.61 | 6.38 | 1.22 | 3.0 GiB |  | 20.6 MiB | 1 | success |
| 179 | tools | csv | 1.0 GiB | diff a b | all | 4 | 7.61 | 6.42 | 1.18 | 3.0 GiB |  | 20.6 MiB | 1 | success |
| 180 | tools | csv | 1.0 GiB | diff a b | all | 5 | 7.61 | 6.37 | 1.23 | 3.0 GiB |  | 20.6 MiB | 1 | success |
| 181 | tools | csv | 1.0 GiB | git diff --no-index a b | all | 1 | 98.6 | 98.39 | 0.20 | 7.8 MiB |  | 201 B | 1 | success |
| 182 | tools | csv | 1.0 GiB | git diff --no-index a b | all | 2 | 97.99 | 97.76 | 0.23 | 7.6 MiB |  | 201 B | 1 | success |
| 183 | tools | csv | 1.0 GiB | git diff --no-index a b | all | 3 | 94.56 | 94.31 | 0.24 | 7.8 MiB |  | 201 B | 1 | success |
| 184 | tools | csv | 1.0 GiB | git diff --no-index a b | all | 4 | 95.52 | 95.28 | 0.22 | 7.6 MiB |  | 201 B | 1 | success |
| 185 | tools | csv | 1.0 GiB | git diff --no-index a b | all | 5 | 94.42 | 94.20 | 0.22 | 7.8 MiB |  | 201 B | 1 | success |
| 186 | tools | csv | 1.0 GiB | git diff --no-index --text a b | all | 1 | 94.25 | 94.01 | 0.23 | 7.8 MiB |  | 0 B | 128 | success |
| 187 | tools | csv | 1.0 GiB | git diff --no-index --text a b | all | 2 | 94.64 | 94.43 | 0.20 | 8.0 MiB |  | 0 B | 128 | success |
| 188 | tools | csv | 1.0 GiB | git diff --no-index --text a b | all | 3 | 95.53 | 95.29 | 0.23 | 7.8 MiB |  | 0 B | 128 | success |
| 189 | tools | csv | 1.0 GiB | git diff --no-index --text a b | all | 4 | 96.42 | 96.16 | 0.25 | 7.8 MiB |  | 0 B | 128 | success |
| 190 | tools | csv | 1.0 GiB | git diff --no-index --text a b | all | 5 | 101.71 | 101.48 | 0.22 | 8.0 MiB |  | 0 B | 128 | success |
| 191 | tools | csv | 1.0 GiB | difft a b | all | 1 |  |  |  |  |  | 0 B |  | oom-kill |
| 192 | tools | csv | 1.0 GiB | difft a b | all | 2 |  |  |  |  |  |  |  | skipped |
| 193 | tools | csv | 1.0 GiB | difft a b | all | 3 |  |  |  |  |  |  |  | skipped |
| 194 | tools | csv | 1.0 GiB | difft a b | all | 4 |  |  |  |  |  |  |  | skipped |
| 195 | tools | csv | 1.0 GiB | difft a b | all | 5 |  |  |  |  |  |  |  | skipped |
| 196 | tools | csv | 1.0 GiB | sort a, sort b, comm -3 | all | 1 | 7.4 | 18.37 | 2.79 | 1.8 GiB |  | 13.6 MiB | 0 | success |
| 197 | tools | csv | 1.0 GiB | sort a, sort b, comm -3 | all | 2 | 7.43 | 18.65 | 2.70 | 1.8 GiB |  | 13.6 MiB | 0 | success |
| 198 | tools | csv | 1.0 GiB | sort a, sort b, comm -3 | all | 3 | 7.38 | 18.50 | 2.70 | 1.8 GiB |  | 13.6 MiB | 0 | success |
| 199 | tools | csv | 1.0 GiB | sort a, sort b, comm -3 | all | 4 | 7.35 | 18.47 | 2.63 | 1.8 GiB |  | 13.6 MiB | 0 | success |
| 200 | tools | csv | 1.0 GiB | sort a, sort b, comm -3 | all | 5 | 7.52 | 18.61 | 2.73 | 1.8 GiB |  | 13.6 MiB | 0 | success |
| 201 | tools | keys | 1.0 GiB | rift --key csv:id | all | 1 | 1.49 | 7.46 | 0.49 | 208.2 MiB |  | 7.7 MiB | 1 | success |
| 202 | tools | keys | 1.0 GiB | rift --key csv:id | all | 2 | 1.49 | 7.44 | 0.53 | 208.2 MiB |  | 7.7 MiB | 1 | success |
| 203 | tools | keys | 1.0 GiB | rift --key csv:id | all | 3 | 1.48 | 7.47 | 0.50 | 208.3 MiB |  | 7.7 MiB | 1 | success |
| 204 | tools | keys | 1.0 GiB | rift --key csv:id | all | 4 | 1.49 | 7.47 | 0.49 | 208.2 MiB |  | 7.7 MiB | 1 | success |
| 205 | tools | keys | 1.0 GiB | rift --key csv:id | all | 5 | 1.49 | 7.51 | 0.48 | 208.0 MiB |  | 7.7 MiB | 1 | success |
| 206 | tools | keys | 1.0 GiB | duckdb full outer join | all | 1 | 3.13 | 26.99 | 4.41 | 3.3 GiB |  | 64 B | 0 | success |
| 207 | tools | keys | 1.0 GiB | duckdb full outer join | all | 2 | 3.23 | 27.12 | 4.61 | 3.3 GiB |  | 64 B | 0 | success |
| 208 | tools | keys | 1.0 GiB | duckdb full outer join | all | 3 | 3.15 | 27.30 | 4.61 | 3.3 GiB |  | 64 B | 0 | success |
| 209 | tools | keys | 1.0 GiB | duckdb full outer join | all | 4 | 3.13 | 27.06 | 4.67 | 3.3 GiB |  | 64 B | 0 | success |
| 210 | tools | keys | 1.0 GiB | duckdb full outer join | all | 5 | 3.18 | 26.98 | 4.70 | 3.3 GiB |  | 64 B | 0 | success |
| 211 | tools | keys | 1.0 GiB | duckdb except | all | 1 |  |  |  |  |  | 0 B |  | oom-kill |
| 212 | tools | keys | 1.0 GiB | duckdb except | all | 2 |  |  |  |  |  |  |  | skipped |
| 213 | tools | keys | 1.0 GiB | duckdb except | all | 3 |  |  |  |  |  |  |  | skipped |
| 214 | tools | keys | 1.0 GiB | duckdb except | all | 4 |  |  |  |  |  |  |  | skipped |
| 215 | tools | keys | 1.0 GiB | duckdb except | all | 5 |  |  |  |  |  |  |  | skipped |
| 216 | tools | keys | 1.0 GiB | csvdiff -p 0 | all | 1 |  |  |  |  |  | 0 B |  | oom-kill |
| 217 | tools | keys | 1.0 GiB | csvdiff -p 0 | all | 2 |  |  |  |  |  |  |  | skipped |
| 218 | tools | keys | 1.0 GiB | csvdiff -p 0 | all | 3 |  |  |  |  |  |  |  | skipped |
| 219 | tools | keys | 1.0 GiB | csvdiff -p 0 | all | 4 |  |  |  |  |  |  |  | skipped |
| 220 | tools | keys | 1.0 GiB | csvdiff -p 0 | all | 5 |  |  |  |  |  |  |  | skipped |
| 221 | tools | keys | 1.0 GiB | qsv diff -k 0 | all | 1 | 4.82 | 12.44 | 0.36 | 375.5 MiB |  | 39.9 MiB | 0 | success |
| 222 | tools | keys | 1.0 GiB | qsv diff -k 0 | all | 2 | 5.22 | 13.09 | 0.40 | 521.5 MiB |  | 39.9 MiB | 0 | success |
| 223 | tools | keys | 1.0 GiB | qsv diff -k 0 | all | 3 | 4.91 | 12.70 | 0.35 | 378.8 MiB |  | 39.9 MiB | 0 | success |
| 224 | tools | keys | 1.0 GiB | qsv diff -k 0 | all | 4 | 4.99 | 12.65 | 0.40 | 438.6 MiB |  | 39.9 MiB | 0 | success |
| 225 | tools | keys | 1.0 GiB | qsv diff -k 0 | all | 5 | 4.77 | 12.39 | 0.35 | 369.7 MiB |  | 39.9 MiB | 0 | success |

## Tool comparison

One row per command and pair. Time is the median wall time of the runs that finished, with the fastest and slowest; RSS is the median peak resident set; counts are what the tool reported, as changed lines (- and +) or keys (+ added, - removed, ~ changed); failed lists the runs the cap killed (oom-kill), the timeout stopped (timeout), that printed no counts (no result, with the exit code; the tool's message is in raw/ID.err), or that were skipped after a kill or a stop. Every tool's version is under Host.

| pair | size | tool | runs | time | fastest | slowest | RSS | peak heap | output | counts | failed |
|---|---|---|---|---|---|---|---|---|---|---|---|
| csv | 256.0 MiB | cat a b | wc -c | 5 of 5 | 0.01 s | 0.01 s | 0.02 s | 7.8 MiB |  | 512.0 MiB |  |  |
| csv | 256.0 MiB | cmp a b | 5 of 5 | 0.00 s | 0.00 s | 0.00 s | 7.9 MiB |  | 97 B |  |  |
| csv | 256.0 MiB | rift --stats --mem 256M | 5 of 5 | 0.22 s | 0.22 s | 0.22 s | 106.3 MiB | 78.2 MiB | 5.4 MiB | -29316 +29694 |  |
| csv | 256.0 MiB | diff a b | 5 of 5 | 1.54 s | 1.52 s | 1.56 s | 764.9 MiB |  | 4.7 MiB | 59144 lines |  |
| csv | 256.0 MiB | git diff --no-index a b | 5 of 5 | 2.53 s | 2.52 s | 2.54 s | 1.0 GiB |  | 5.5 MiB | -29316 +29694 |  |
| csv | 256.0 MiB | git diff --no-index --text a b | 5 of 5 | 2.54 s | 2.52 s | 2.54 s | 1.0 GiB |  | 5.5 MiB | -29316 +29694 |  |
| csv | 256.0 MiB | difft a b | 5 of 5 | 10.56 s | 10.49 s | 10.59 s | 3.6 GiB |  | 6.5 MiB |  |  |
| csv | 256.0 MiB | sort a, sort b, comm -3 | 5 of 5 | 1.69 s | 1.69 s | 1.70 s | 337.4 MiB |  | 3.2 MiB | 41044 lines |  |
| csv | 1.0 GiB | cat a b | wc -c | 5 of 5 | 0.05 s | 0.05 s | 0.05 s | 7.7 MiB |  | 2.0 GiB |  |  |
| csv | 1.0 GiB | cmp a b | 5 of 5 | 0.00 s | 0.00 s | 0.00 s | 7.8 MiB |  | 97 B |  |  |
| csv | 1.0 GiB | rift --stats --mem 256M | 5 of 5 | 0.55 s | 0.55 s | 0.56 s | 107.8 MiB | 78.8 MiB | 23.5 MiB | -127381 +131588 |  |
| csv | 1.0 GiB | diff a b | 5 of 5 | 7.61 s | 7.61 s | 7.66 s | 3.0 GiB |  | 20.6 MiB | 260045 lines |  |
| csv | 1.0 GiB | git diff --no-index a b | 5 of 5 | 95.52 s | 94.42 s | 98.60 s | 7.8 MiB |  | 201 B | -0 +0 |  |
| csv | 1.0 GiB | git diff --no-index --text a b | 5 of 5 | 95.53 s | 94.25 s | 101.71 s | 7.8 MiB |  | 0 B | -0 +0 |  |
| csv | 1.0 GiB | difft a b | 0 of 5 |  |  |  |  |  |  |  | oom-kill, skipped, skipped, skipped, skipped |
| csv | 1.0 GiB | sort a, sort b, comm -3 | 5 of 5 | 7.40 s | 7.35 s | 7.52 s | 1.8 GiB |  | 13.6 MiB | 175253 lines |  |
| keys | 256.0 MiB | rift --key csv:id | 5 of 5 | 0.36 s | 0.36 s | 0.36 s | 72.7 MiB |  | 1.9 MiB | +34197 -34510 ~31447 |  |
| keys | 256.0 MiB | duckdb full outer join | 5 of 5 | 0.73 s | 0.73 s | 0.73 s | 1.2 GiB |  | 61 B | +34197 -34510 ~31447 (memory_limit 4.3 GiB) |  |
| keys | 256.0 MiB | duckdb full outer join, memory_limit 256MB | 0 of 5 |  |  |  |  |  |  |  | no result (exit 1), no result (exit 1), no result (exit 1), no result (exit 1), no result (exit 1) |
| keys | 256.0 MiB | duckdb except | 5 of 5 | 1.33 s | 1.32 s | 1.34 s | 1.6 GiB |  | 47 B | rows only in A 65957, only in B 65644 (memory_limit 4.3 GiB) |  |
| keys | 256.0 MiB | duckdb except, memory_limit 256MB | 0 of 5 |  |  |  |  |  |  |  | no result (exit 1), no result (exit 1), no result (exit 1), no result (exit 1), no result (exit 1) |
| keys | 256.0 MiB | csvdiff -p 0 | 5 of 5 | 3.22 s | 3.14 s | 3.26 s | 1.8 GiB |  | 10.3 MiB | +34197 -34510 ~31447 |  |
| keys | 256.0 MiB | qsv diff -k 0 | 5 of 5 | 1.15 s | 1.13 s | 1.21 s | 129.3 MiB |  | 10.3 MiB | rows + 65644, - 65957 |  |
| keys | 256.0 MiB | daff diff --id id | 0 of 5 |  |  |  |  |  |  |  | oom-kill, skipped, skipped, skipped, skipped |
| keys | 1.0 GiB | rift --key csv:id | 5 of 5 | 1.49 s | 1.48 s | 1.49 s | 208.2 MiB |  | 7.7 MiB | +123949 -125977 ~128633 |  |
| keys | 1.0 GiB | duckdb full outer join | 5 of 5 | 3.15 s | 3.13 s | 3.23 s | 3.3 GiB |  | 64 B | +123949 -125977 ~128633 (memory_limit 3.1 GiB) |  |
| keys | 1.0 GiB | duckdb except | 0 of 5 |  |  |  |  |  |  |  | oom-kill, skipped, skipped, skipped, skipped |
| keys | 1.0 GiB | csvdiff -p 0 | 0 of 5 |  |  |  |  |  |  |  | oom-kill, skipped, skipped, skipped, skipped |
| keys | 1.0 GiB | qsv diff -k 0 | 5 of 5 | 4.91 s | 4.77 s | 5.22 s | 378.8 MiB |  | 39.9 MiB | rows + 252582, - 254610 |  |
| sql | 256.0 MiB | cat a b | wc -c | 5 of 5 | 0.01 s | 0.01 s | 0.01 s | 7.8 MiB |  | 512.2 MiB |  |  |
| sql | 256.0 MiB | cmp a b | 5 of 5 | 0.00 s | 0.00 s | 0.00 s | 7.8 MiB |  | 95 B |  |  |
| sql | 256.0 MiB | rift --stats --mem 256M | 5 of 5 | 0.22 s | 0.22 s | 0.22 s | 108.4 MiB | 79.0 MiB | 5.9 MiB | -30721 +35651 |  |
| sql | 256.0 MiB | diff a b | 5 of 5 | 1.61 s | 1.61 s | 1.63 s | 776.2 MiB |  | 5.2 MiB | 67228 lines |  |
| sql | 256.0 MiB | git diff --no-index a b | 5 of 5 | 2.58 s | 2.58 s | 2.60 s | 1.0 GiB |  | 6.0 MiB | -30721 +35651 |  |
| sql | 256.0 MiB | git diff --no-index --text a b | 5 of 5 | 2.59 s | 2.58 s | 2.60 s | 1.0 GiB |  | 6.0 MiB | -30721 +35651 |  |
| sql | 256.0 MiB | difft a b | 5 of 5 | 11.80 s | 11.76 s | 11.87 s | 3.6 GiB |  | 9.4 MiB |  |  |
| sql | 256.0 MiB | sort a, sort b, comm -3 | 5 of 5 | 1.97 s | 1.97 s | 1.97 s | 340.9 MiB |  | 3.5 MiB | 46476 lines |  |
| sql | 1.0 GiB | cat a b | wc -c | 5 of 5 | 0.05 s | 0.05 s | 0.05 s | 7.8 MiB |  | 2.0 GiB |  |  |
| sql | 1.0 GiB | cmp a b | 5 of 5 | 0.00 s | 0.00 s | 0.00 s | 7.8 MiB |  | 95 B |  |  |
| sql | 1.0 GiB | rift --stats --mem 256M | 5 of 5 | 0.55 s | 0.55 s | 0.55 s | 109.5 MiB | 79.4 MiB | 23.0 MiB | -122750 +133446 |  |
| sql | 1.0 GiB | diff a b | 5 of 5 | 7.67 s | 7.65 s | 7.70 s | 3.0 GiB |  | 20.1 MiB | 256880 lines |  |
| sql | 1.0 GiB | git diff --no-index a b | 5 of 5 | 42.58 s | 42.51 s | 43.28 s | 7.8 MiB |  | 201 B | -0 +0 |  |
| sql | 1.0 GiB | git diff --no-index --text a b | 5 of 5 | 42.50 s | 42.33 s | 42.92 s | 7.9 MiB |  | 0 B | -0 +0 |  |
| sql | 1.0 GiB | difft a b | 0 of 5 |  |  |  |  |  |  |  | oom-kill, skipped, skipped, skipped, skipped |
| sql | 1.0 GiB | sort a, sort b, comm -3 | 5 of 5 | 8.42 s | 8.40 s | 8.45 s | 1.8 GiB |  | 13.5 MiB | 177216 lines |  |
