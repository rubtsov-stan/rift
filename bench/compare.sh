#!/bin/sh
# Runs two rift builds interleaved on the reference pairs and prints a
# markdown table: median and min-max wall time, peak heap, load, verdict.
#
#   bench/compare.sh BASE HEAD SCRATCH
#
# BASE and HEAD are each a git ref (built from `git archive` into SCRATCH) or
# an executable rift binary. The report names a ref by its commit and never
# prints a binary's path: set BASE_COMMIT and HEAD_COMMIT to the commits the
# binaries were built from, or it says "unknown commit". rift-gen comes from HEAD's build, or from the
# directory of HEAD's binary. SCRATCH holds the pairs, builds and outputs; it
# is never inside the repository and nothing in it is committed.
#
# RUNS=N changes the 10 timed runs per side. Each side gets one untimed
# warm-up first, then runs alternate base, head, base, head, ...
#
# SORTED=1 accepts a head that prints the base's lines in another order: the
# warm-ups are compared as sorted lines, and every timed run byte for byte
# against its own side's warm-up, so each side must still be deterministic.
#
# Takes the lock directory gate.lock in the repository's git directory for
# the whole run (builds, data generation, measurement), so no build or test
# that takes the same lock runs during a measurement; waits while it is held.
#
# Exit codes: 0 done (a verdict of "slower" or "heap higher" is in the
# table, not in the exit code), 1 outputs differ or a run failed.

# `set -- $(stats ...)` splits three numbers into $1 $2 $3 on purpose.
# shellcheck disable=SC2046
set -eu

[ $# -eq 3 ] || { sed -n '2,/^set/p' "$0" >&2; exit 2; }
base=$1 head=$2 scratch=$3
runs=${RUNS:-10}
sorted=${SORTED:-0}
root=$(cd "$(dirname "$0")/.." && pwd)
# --git-common-dir can be relative to $root, so resolve it from there.
lock=$(cd "$root" && cd "$(git rev-parse --git-common-dir)" && pwd)/gate.lock

# Pair name | rift-gen args | rift args. Seed 7, 0.1% changes.
pairs='sql-256M|--profile sql --size 256M|
csv-256M|--profile csv --size 256M|
sql-1G|--profile sql --size 1G|
csv-1G|--profile csv --size 1G|
shuffled-csv-key-256M|--profile shuffled --rows csv --size 256M|--key csv:id
periodic1000-256M|--profile periodic --period 1000 --size 256M|
periodic1000-1G|--profile periodic --period 1000 --size 1G|
nokey-4M|--profile nokey --size 4M|
ndjson-500B-256M|--profile ndjson --size 256M --line-bytes 500|
ndjson-2K-256M|--profile ndjson --size 256M --line-bytes 2048|'

# PAIRS replaces the list above, for a quick check of the script itself.
pairs=${PAIRS:-$pairs}

got=0
release() {
  [ "$got" = 1 ] || return 0
  got=0
  rmdir "$lock"
}
trap release EXIT
trap 'exit 130' INT TERM
until mkdir "$lock" 2>/dev/null; do echo "waiting for $lock" >&2; sleep 30; done
got=1
mkdir -p "$scratch"
scratch=$(cd "$scratch" && pwd)
case $scratch in "$root"/*) echo "SCRATCH must be outside the repository" >&2; exit 2 ;; esac

# build REF NAME: prints the path of rift (and leaves rift-gen beside it).
build() {
  if [ -x "$1" ] && [ -f "$1" ]; then echo "$1"; return; fi
  src="$scratch/src-$2"
  rm -rf "$src"; mkdir -p "$src"
  git -C "$root" archive "$1" | tar -x -C "$src"
  CARGO_TARGET_DIR="$scratch/target-$2" cargo build --release --manifest-path "$src/Cargo.toml" \
    -p rift-diff -p rift-gen >&2
  echo "$scratch/target-$2/release/rift"
}
rbase=$(build "$base" base)
rhead=$(build "$head" head)
# label ARG BIN COMMIT: the commit a ref resolves to, or for a binary the
# given commit, its --version and the start of its sha256; never a path.
label() {
  if [ "$1" != "$2" ]; then echo "$(git -C "$root" rev-parse --short "$1^{commit}") ($1)"; return; fi
  sum=$( { shasum -a 256 "$2" 2>/dev/null || sha256sum "$2"; } | cut -c1-12)
  echo "${3:-unknown commit}, binary $("$2" --version), sha256 $sum"
}
lbase=$(label "$base" "$rbase" "${BASE_COMMIT:-}")
lhead=$(label "$head" "$rhead" "${HEAD_COMMIT:-}")
gen=$(dirname "$rhead")/rift-gen
[ -x "$gen" ] || { echo "no rift-gen beside $rhead" >&2; exit 2; }

# Load average (1 min) now.
load() { uptime | awk -F'load averages?: ' '{print $2}' | tr -d ',' | awk '{print $1}'; }
top() { ps -Ao pcpu,comm -r | sed -n '2,6p'; }
human() { awk -v b="$1" 'BEGIN { split("B KiB MiB GiB", u); i = 1; while (b >= 1024 && i < 4) { b /= 1024; i++ } printf "%.1f %s", b, u[i] }'; }
# stats FILE: median, min, max of the numbers in FILE.
stats() { sort -n "$1" | awk '{v[NR]=$1} END {m = (NR % 2) ? v[(NR+1)/2] : (v[NR/2]+v[NR/2+1])/2; printf "%.4f %.4f %.4f", m, v[1], v[NR]}'; }

# one BIN RIFTARGS A B OUT: times one run; wall seconds into $scratch/wall,
# stdout into OUT, stderr (minus the time and heap lines) into OUT.err.
one() {
  bin=$1 rargs=$2 a=$3 b=$4 out=$5
  rc=0
  # $rargs is split on purpose: it holds flags only.
  # shellcheck disable=SC2086
  perl -MTime::HiRes=time -e '
    open(W, ">&=", 3) or die; $t = time;
    $r = system(@ARGV); printf W "%.4f\n", time - $t; exit($r >> 8)' \
    "$bin" --stats $rargs "$a" "$b" < /dev/null > "$out" 2> "$out.raw" 3> "$scratch/wall" || rc=$?
  if [ "$rc" -gt 1 ]; then echo "run failed: $bin $rargs (exit $rc)" >&2; cat "$out.raw" >&2; exit 1; fi
  heap=$(grep -o 'peak heap [^(]*([0-9]* bytes)' "$out.raw" | tail -n1 | sed 's/.*(\([0-9]*\) bytes)/\1/')
  [ -n "$heap" ] || { echo "no peak heap in stderr of $bin" >&2; exit 1; }
  echo "$heap" > "$out.heap"
  sed -e 's/peak heap.*//' "$out.raw" | grep -v '^time ' > "$out.err" || true
}
same() { # A B WHAT: fails loudly when the outputs differ byte for byte.
  cmp -s "$1" "$2" && cmp -s "$1.err" "$2.err" && return 0
  echo "OUTPUT DIFFERS ($3): base and head must print the same bytes" >&2
  cmp "$1" "$2" >&2 || true; cmp "$1.err" "$2.err" >&2 || true
  exit 1
}
same_lines() { # A B WHAT: the same lines in any order; stderr byte for byte.
  LC_ALL=C sort "$1" > "$1.sorted"; LC_ALL=C sort "$2" > "$2.sorted"
  cmp -s "$1.sorted" "$2.sorted" && cmp -s "$1.err" "$2.err" && return 0
  echo "OUTPUT DIFFERS ($3): base and head must print the same lines" >&2
  cmp "$1.sorted" "$2.sorted" >&2 || true; cmp "$1.err" "$2.err" >&2 || true
  exit 1
}
if [ "$sorted" = 1 ]; then
  check=same_lines outputs="the same lines as base's (sorted), and each run the same bytes as its own side's warm-up"
else
  check=same outputs="byte-identical"
fi

cpu=$(sysctl -n machdep.cpu.brand_string 2>/dev/null || awk -F': ' '/model name/ {print $2; exit}' /proc/cpuinfo)
ram=$(sysctl -n hw.memsize 2>/dev/null || echo $(( $(awk '/MemTotal/ {print $2}' /proc/meminfo) * 1024 )))
echo "== start $(date -u +%FT%TZ), load $(load); top CPU users:" >&2
top >&2
start_top=$(top)
start_date=$(date -u +'%F %T UTC')

table='| Pair | base median (min-max) s | head median (min-max) s | head vs base | threshold | peak heap base / head | load (1 min) | verdict |
|---|---|---|---|---|---|---|---|'
while IFS='|' read -r name gargs rargs; do
  d="$scratch/pairs/$name"
  if [ ! -f "$d/manifest.tsv" ]; then
    rm -rf "$d.part"; mkdir -p "$scratch/pairs"
    # shellcheck disable=SC2086
    "$gen" --seed 7 --change-ratio 0.001 $gargs --out "$d.part" >&2
    mv "$d.part" "$d"
  fi
  echo "== $name" >&2
  w="$scratch/w-$name"; mkdir -p "$w"
  : > "$w/base.t"; : > "$w/head.t"; : > "$w/load"
  one "$rbase" "$rargs" "$d/a" "$d/b" "$w/base.out"
  one "$rhead" "$rargs" "$d/a" "$d/b" "$w/head.out"
  "$check" "$w/base.out" "$w/head.out" "$name, warm-up"
  i=0
  while [ "$i" -lt "$runs" ]; do
    i=$((i + 1))
    for side in base head; do
      eval "bin=\$r$side"
      load >> "$w/load"
      one "$bin" "$rargs" "$d/a" "$d/b" "$w/$side.out.$i"
      load >> "$w/load"
      cat "$scratch/wall" >> "$w/$side.t"
      same "$w/$side.out" "$w/$side.out.$i" "$name, run $i, $side"
      hb=$(cat "$w/$side.out.$i.heap"); echo "$hb" >> "$w/$side.h"
      rm -f "$w/$side.out.$i" "$w/$side.out.$i".*
    done
  done
  set -- $(stats "$w/base.t"); bm=$1 bmin=$2 bmax=$3
  set -- $(stats "$w/head.t"); hm=$1 hmin=$2 hmax=$3
  set -- $(stats "$w/base.h"); bheap=$3
  set -- $(stats "$w/head.h"); hheap=$3
  lr=$(sort -n "$w/load" | awk 'NR==1 {lo=$1} {hi=$1} END {print lo "-" hi}')
  # Threshold: the larger of 3% and the combined spread, the mean of the two
  # (max-min) ranges, both relative to base's median. Slower only above it.
  verdict=$(awk -v bm="$bm" -v hm="$hm" -v r1="$(echo "$bmax $bmin" | awk '{print $1-$2}')" \
    -v r2="$(echo "$hmax $hmin" | awk '{print $1-$2}')" -v bh="$bheap" -v hh="$hheap" 'BEGIN {
      d = (hm - bm) / bm * 100; sp = (r1 + r2) / 2 / bm * 100; th = sp > 3 ? sp : 3
      v = d > th ? "SLOWER" : "not slower"; if (hh > bh) v = v ", HEAP HIGHER"
      printf "%+.1f%%|%.1f%%|%s", d, th, v }')
  d1=${verdict%%|*}; rest=${verdict#*|}; th=${rest%%|*}; v=${rest#*|}
  table="$table
| $name | $bm ($bmin-$bmax) | $hm ($hmin-$hmax) | $d1 | $th | $(human "$bheap") / $(human "$hheap") | $lr | $v |"
  rm -rf "$w"
done <<PAIRS
$pairs
PAIRS

end_top=$(top)
cat <<OUT
### Machine

- CPU: $cpu, RAM $(human "$ram"), $(uname -sr)
- base: $lbase
- head: $lhead
- Warm cache: nothing is dropped; each pair is read once before its timed runs.
- Started $start_date, ended $(date -u +'%F %T UTC')
- Load average (1 min) sampled before and after every run; the load column is the range for that pair.

### Command

\`bench/compare.sh BASE HEAD SCRATCH\` with RUNS=$runs: per pair one warm-up per side, then $runs runs per side interleaved base, head, base, head, ...; \`rift --stats --mem 256M [--key csv:id] a b\`, stdout to a file; rift-gen seed 7, --change-ratio 0.001. Outputs are compared with cmp (stdout, and stderr without the time and peak heap lines): head's warm-up against base's$( [ "$sorted" = 1 ] && echo ' as sorted lines (SORTED=1)'), every run against its own side's warm-up.

### Result

$table

Verdict: "SLOWER" if the head median exceeds the base median by more than the larger of 3% and the combined spread (mean of the two max-min ranges over base's median); "HEAP HIGHER" if the head's peak heap exceeds base's. All outputs were $outputs.

### Top CPU users

At start:

$start_top

At end:

$end_top
OUT
