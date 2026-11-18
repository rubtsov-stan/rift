## Apple M1, v0.3.4 against origin/main (6b80068), 2026-11-18

Both sides are the same code, so every row should read "not slower". This checks the harness; it is not a performance result. Load stayed at 4 to 6 during the runs, so the spreads are wide (up to 63% of a median).

### Machine

- CPU: Apple M1, RAM 16.0 GiB, Darwin 27.0.0
- base: v0.3.4
- head: origin/main
- Warm cache: nothing is dropped; each pair is read once before its timed runs.
- Started 2026-11-18 02:30:32 UTC, ended 2026-11-18 02:34:35 UTC
- Load average (1 min) sampled before and after every run; the load column is the range for that pair.

### Command

`bench/compare.sh v0.3.4 origin/main SCRATCH` with RUNS=10: per pair one warm-up per side, then 10 runs per side interleaved base, head, base, head, ...; `rift --stats --mem 256M [--key csv:id] a b`, stdout to a file; rift-gen seed 7, --change-ratio 0.001. Outputs of every run are compared with cmp against base's warm-up output (stdout and stderr without the time and peak heap lines).

### Result

| Pair | base median (min-max) s | head median (min-max) s | head vs base | threshold | peak heap base / head | load (1 min) | verdict |
|---|---|---|---|---|---|---|---|
| sql-256M | 0.2525 (0.2107-0.3025) | 0.2524 (0.2174-0.2885) | -0.0% | 32.3% | 79.1 MiB / 79.1 MiB | 5.01-5.36 | not slower |
| csv-256M | 0.2470 (0.2181-0.3310) | 0.2768 (0.2106-0.3857) | +12.1% | 58.3% | 78.2 MiB / 78.2 MiB | 5.01-5.09 | not slower |
| sql-1G | 0.7594 (0.7186-0.8847) | 0.7581 (0.7425-1.1831) | -0.2% | 39.9% | 79.4 MiB / 79.4 MiB | 4.54-5.58 | not slower |
| csv-1G | 0.7135 (0.6624-0.7901) | 0.6881 (0.6619-0.8061) | -3.6% | 19.1% | 78.8 MiB / 78.8 MiB | 4.95-5.62 | not slower |
| shuffled-csv-key-256M | 0.4515 (0.3679-0.5795) | 0.4726 (0.3709-0.7303) | +4.7% | 63.2% | 74.3 MiB / 74.3 MiB | 5.57-6.25 | not slower |
| periodic1000-256M | 0.9784 (0.9107-1.1615) | 0.9908 (0.9381-1.0818) | +1.3% | 20.2% | 90.7 MiB / 90.7 MiB | 5.32-6.09 | not slower |
| periodic1000-1G | 5.7134 (5.5601-6.6915) | 5.7797 (5.6658-6.1736) | +1.2% | 14.3% | 92.7 MiB / 92.7 MiB | 4.01-5.56 | not slower |
| nokey-4M | 0.0627 (0.0609-0.0657) | 0.0633 (0.0610-0.0699) | +1.0% | 10.9% | 24.7 MiB / 24.7 MiB | 4.01-4.41 | not slower |

Verdict: "SLOWER" if the head median exceeds the base median by more than the larger of 3% and the combined spread (mean of the two max-min ranges over base's median); "HEAP HIGHER" if the head's peak heap exceeds base's. All outputs were byte-identical.

### Top CPU users

At start:

 23.1 /System/Library/PrivateFrameworks/SkyLight.framework/Resources/WindowServer
 18.8 /System/Library/Frameworks/WebKit.framework/Versions/A/XPCServices/com.apple.WebKit.WebContent.xpc/Contents/MacOS/com.apple.WebKit.WebContent
  9.6 /Applications/iTerm.app/Contents/MacOS/iTerm2
  8.2 (a CLI process)
  7.9 /System/Library/Frameworks/CoreServices.framework/Versions/A/Frameworks/FSEvents.framework/Versions/A/Support/fseventsd

At end:

 14.2 /Applications/iTerm.app/Contents/MacOS/iTerm2
  7.9 /bin/sh
  3.2 (a CLI process)
  1.0 /usr/libexec/opendirectoryd
  0.9 /usr/sbin/notifyd
