#!/bin/sh
# Compares rift, GNU diff and git diff on generated or existing pairs and
# prints a markdown report (hardware, commands, results) on stdout.
#
#   bench/run.sh [-s SEED] [-p PROFILE] [-m MEM] [-d SCRATCH] ITEM...
#
# ITEM is a size accepted by rift-gen (16M, 1G) or a directory that holds a
# and b (for example ~/rift-data/gen10g). Sizes are generated into SCRATCH
# (default: a temp directory, removed on exit). Defaults: seed 7, profile
# sql, mem 256M.
#
# Cache: with root, the page cache is dropped before every run (purge on
# macOS, drop_caches on Linux) and rows say "cold". Otherwise rows say "warm".
# GNU diff and git diff are skipped when a+b exceed half of RAM.
set -eu

seed=7 profile=sql mem=256M scratch=
while getopts s:p:m:d: o; do
  case $o in
    s) seed=$OPTARG ;; p) profile=$OPTARG ;; m) mem=$OPTARG ;; d) scratch=$OPTARG ;;
    *) sed -n '2,/^set/p' "$0" >&2; exit 2 ;;
  esac
done
shift $((OPTIND - 1))
[ $# -gt 0 ] || { echo "usage: $0 [-s SEED] [-p PROFILE] [-m MEM] [-d SCRATCH] SIZE|DIR..." >&2; exit 2; }

root=$(cd "$(dirname "$0")/.." && pwd)
rift="$root/target/release/rift"
gen="$root/target/release/rift-gen"
cargo build --release --manifest-path "$root/Cargo.toml" -p rift-diff -p rift-gen >&2

if [ -z "$scratch" ]; then
  scratch=$(mktemp -d "${TMPDIR:-/tmp}/rift-bench.XXXXXX")
  trap 'rm -rf "$scratch"' EXIT
else
  mkdir -p "$scratch"
fi

os=$(uname -s)
if [ "$os" = Darwin ]; then
  timecmd="/usr/bin/time -l"
  ram=$(sysctl -n hw.memsize)
  cpu=$(sysctl -n machdep.cpu.brand_string)
  osname="macOS $(sw_vers -productVersion)"
else
  timecmd="/usr/bin/time -v"
  ram=$((  $(awk '/MemTotal/ {print $2}' /proc/meminfo) * 1024 ))
  cpu=$(awk -F': ' '/model name/ {print $2; exit}' /proc/cpuinfo)
  osname=$(. /etc/os-release 2>/dev/null && echo "$PRETTY_NAME"; true)
  osname="${osname:-Linux} $(uname -r)"
fi

disk() {
  dev=$(df -P "$scratch" | awk 'NR==2 {print $1}')
  if [ "$os" = Darwin ]; then
    # APFS volumes are synthesized, so ask for the NVMe model.
    # Takes the first NVMe device; wrong on machines with several disks.
    system_profiler SPNVMeDataType 2>/dev/null | awk -F': ' '/Model:/ {print $2; exit}'
  else
    lsblk -no MODEL "$dev" 2>/dev/null | head -n1
  fi
}
diskmodel=$(disk || true)

cache=warm
if [ "$(id -u)" -eq 0 ]; then cache=cold; fi
drop() {
  [ "$cache" = cold ] || return 0
  if [ "$os" = Darwin ]; then purge; else sync; echo 3 > /proc/sys/vm/drop_caches; fi
}

# GNU diff: gdiff, else diff when `diff --version` says GNU.
gnu=
if command -v gdiff >/dev/null 2>&1; then gnu=gdiff
elif diff --version 2>/dev/null | head -n1 | grep -q GNU; then gnu=diff
fi

human() { awk -v b="$1" 'BEGIN { split("B KiB MiB GiB TiB", u); i = 1; while (b >= 1024 && i < 5) { b /= 1024; i++ } printf (i == 1 ? "%d %s" : "%.1f %s"), b, u[i] }'; }

# row LABEL TOOL CMD... : runs CMD under time, appends a table row.
# stdout of CMD goes to $scratch/out, time's report lands in $scratch/err.
row() {
  pair=$1 tool=$2; shift 2
  drop
  rc=0
  $timecmd "$@" > "$scratch/out" 2> "$scratch/err" || rc=$?
  if [ "$rc" -gt 1 ]; then
    echo "| $pair | $tool | $cache | failed | | | | exit $rc |"
    return 0
  fi
  if [ "$os" = Darwin ]; then
    wall=$(awk '/ real / {print $1}' "$scratch/err")
    rss=$(awk '/maximum resident set size/ {print $1}' "$scratch/err")
  else
    wall=$(awk -F': ' '/Elapsed/ {n = split($NF, t, ":"); s = 0; for (i = 1; i <= n; i++) s = s * 60 + t[i]; print s}' "$scratch/err")
    rss=$(awk -F': ' '/Maximum resident/ {print $2 * 1024}' "$scratch/err")
  fi
  size=$(wc -c < "$scratch/out" | tr -d ' ')
  heap=
  case $tool in
    # "peak heap 9.8 MiB of 64.0 MiB": on stdout for --summary, on stderr
    # for --stats, where the diff itself is stdout.
    rift*summary*) heap=$(awk '/^peak heap/ {print $3, $4}' "$scratch/out") ;;
    rift*stats*) heap=$(awk '/^peak heap/ {print $3, $4}' "$scratch/err") ;;
  esac
  echo "| $pair | $tool | $cache | $wall s | $(human "$rss") | $(human "$size") | $heap | exit $rc |"
}

skip() { echo "| $1 | $2 | | not run: $3 | | | | |"; }

hw="- CPU: $cpu
- RAM: $(human "$ram")
- Disk: ${diskmodel:-unknown}
- OS: $osname
- rift: $(git -C "$root" rev-parse --short HEAD) release profile, no target-cpu=native
- diff: ${gnu:-$(diff --version 2>&1 | head -n1) (not GNU; run and labelled as such)}
- git: $(git --version)
- Page cache: $cache$( [ "$cache" = warm ] && echo ' (no root: caches not dropped)')"

body=
cmds=
seeds=
for item in "$@"; do
  if [ -d "$item" ]; then
    d=$item label=$(basename "$item")
    seeds="$seeds
- $label: existing pair in $item (bench/data.sh uses seed 7)"
  else
    label="$item $profile seed $seed"
    d="$scratch/gen-$item-$profile-$seed"
    if [ ! -f "$d/manifest.tsv" ]; then
      rm -rf "$d.part"
      "$gen" --seed "$seed" --size "$item" --profile "$profile" --out "$d.part" >&2
      mv "$d.part" "$d"
    fi
    seeds="$seeds
- $item $profile: seed $seed (rift-gen --seed $seed --size $item --profile $profile --out DIR)"
  fi
  a="$d/a" b="$d/b"
  total=$(( $(wc -c < "$a") + $(wc -c < "$b") ))
  echo "== $label ($(human "$total"))" >&2

  body="$body
$(row "$label" "rift --summary --mem $mem" "$rift" --summary --mem "$mem" "$a" "$b")"

  body="$body
$(row "$label" "rift --stats --mem $mem" "$rift" --stats --mem "$mem" "$a" "$b")"

  if [ $((total * 2)) -gt "$ram" ]; then
    why="needs more memory than available ($(human "$total") of input, $(human "$ram") RAM)"
    body="$body
$(skip "$label" "diff" "$why")
$(skip "$label" "git diff --no-index" "$why")"
  else
    if [ -n "$gnu" ]; then dname="GNU diff $gnu"
    else dname="system diff (not GNU)"
    fi
    body="$body
$(row "$label" "$dname" "${gnu:-diff}" "$a" "$b")"
    body="$body
$(row "$label" "git diff --no-index a b" git diff --no-index "$a" "$b")"
  fi
done

cat <<OUT
### Hardware

$hw

### Seeds and commands
$seeds
- summary: \`rift --summary --mem $mem a b\`; full diff: \`rift --stats --mem $mem a b\`
- diff: \`diff a b\` (GNU when labelled so); git: \`git diff --no-index a b\`
- timing: \`$timecmd COMMAND\`, stdout redirected to a file (output column is its size)

### Results

| Pair | Command | Cache | Wall | Peak RSS | Output | Peak heap | Status |
|---|---|---|---|---|---|---|---|$body
OUT
