## Apple M1, 1 GiB and 10 GiB, 2026-11-15

- Command: `bench/run.sh 1G 10G` from the repository root, main at `e06f3d4`.
- Started 2026-11-14 23:16:44 UTC, ended 23:19:55 UTC (2026-11-15 local time).
- Load average (1 min) 1.98 at the start, 3.41 at the end; nothing else of the benchmark's ran.
- One run of each command. The 10 GiB pair is 20 GiB of input against 16 GiB of RAM, so not all of it was in the page cache.

### Hardware

- CPU: Apple M1
- RAM: 16.0 GiB
- Disk: APPLE SSD AP0512Q
- OS: macOS 27.0.1
- rift: e06f3d4 release profile, no target-cpu=native
- diff: Apple diff (based on FreeBSD diff) (not GNU; run and labelled as such)
- git: git version 2.50.1 (Apple Git-155)
- Page cache: warm (no root: caches not dropped)

### Seeds and commands

- 1G sql: seed 7 (rift-gen --seed 7 --size 1G --profile sql --out DIR)
- 10G sql: seed 7 (rift-gen --seed 7 --size 10G --profile sql --out DIR)
- summary: `rift --summary --mem 256M a b`; full diff: `rift --stats --mem 256M a b`
- diff: `diff a b` (GNU when labelled so); git: `git diff --no-index a b`
- timing: `/usr/bin/time -l COMMAND`, stdout redirected to a file (output column is its size)

### Results

| Pair | Command | Cache | Wall | Peak RSS | Output | Peak heap | Status |
|---|---|---|---|---|---|---|---|
| 1G sql seed 7 | rift --summary --mem 256M | warm | 1.03 s | 124.2 MiB | 1.0 KiB | 79.4 MiB | exit 1 |
| 1G sql seed 7 | rift --stats --mem 256M | warm | 0.91 s | 127.2 MiB | 23.0 MiB | 79.4 MiB | exit 1 |
| 1G sql seed 7 | system diff (not GNU) | warm | 17.95 s | 3.1 GiB | 22.9 MiB |  | exit 1 |
| 1G sql seed 7 | git diff --no-index a b | warm | 63.92 s | 4.9 MiB | 573 B |  | exit 1 |
| 10G sql seed 7 | rift --summary --mem 256M | warm | 14.25 s | 132.8 MiB | 1.1 KiB | 92.4 MiB | exit 1 |
| 10G sql seed 7 | rift --stats --mem 256M | warm | 25.48 s | 132.9 MiB | 238.2 MiB | 92.4 MiB | exit 1 |
| 10G sql seed 7 | diff | | not run: needs more memory than available (20.0 GiB of input, 16.0 GiB RAM) | | | | |
| 10G sql seed 7 | git diff --no-index | | not run: needs more memory than available (20.0 GiB of input, 16.0 GiB RAM) | | | | |
