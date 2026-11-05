#!/bin/sh
# CI memory gate: generates a pair, runs `rift --summary --mem MEM` and the
# full diff `rift --stats --mem MEM`, and fails when the counting
# allocator's peak heap of either exceeds MEM.
#
#   bench/ci-mem.sh [SIZE] [MEM]     defaults 1G and 256M
#
# rift prints the peak with one decimal in the unit that fits: on stdout
# for --summary, on stderr for --stats, so the diff on stdout stays a
# patch. At 256M that unit is MiB, so the comparison is exact to 0.05 MiB.
set -eu

size=${1:-1G} mem=${2:-256M}
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/rift-mem.XXXXXX")
trap 'rm -rf "$work"' EXIT

"$root/target/release/rift-gen" --seed 7 --size "$size" --profile sql --out "$work/pair"

case $(uname -s) in Darwin) tf=-l ;; *) tf=-v ;; esac

# mem is digits plus K/M/G/T with optional iB, as rift --mem accepts
limit=$(awk -v m="$mem" 'BEGIN {
  n = m + 0; sub(/iB$|B$/, "", m); u = toupper(substr(m, length(m)))
  k = (u == "K") ? 1024 : (u == "M") ? 1048576 : (u == "G") ? 1073741824 : (u == "T") ? 1099511627776 : 1
  printf "%.0f", n * k }')

# check NAME REPORT: REPORT holds rift's "peak heap" line; err holds time's.
check() {
  if [ "$tf" = -l ]; then
    rss=$(awk '/maximum resident set size/ {print $1}' "$work/err")
  else
    rss=$(awk -F': ' '/Maximum resident/ {print $2 * 1024}' "$work/err")
  fi
  peak=$(awk '/^peak heap/ {
    m = 1; if ($4 == "KiB") m = 1024; else if ($4 == "MiB") m = 1048576
    else if ($4 == "GiB") m = 1073741824; else if ($4 == "TiB") m = 1099511627776
    printf "%.0f", $3 * m }' "$2")
  [ -n "$peak" ] || { echo "$1: no 'peak heap' line in rift output" >&2; exit 1; }
  echo "$1: peak heap $peak bytes, limit $limit bytes, peak RSS $rss bytes"
  if [ "$peak" -gt "$limit" ]; then
    echo "$1: peak heap exceeds --mem" >&2
    exit 1
  fi
}

rc=0
/usr/bin/time $tf "$root/target/release/rift" --summary --mem "$mem" \
  "$work/pair/a" "$work/pair/b" > "$work/out" 2> "$work/err" || rc=$?
cat "$work/out"
[ "$rc" -le 1 ] || { cat "$work/err" >&2; echo "rift --summary failed with exit $rc" >&2; exit 1; }
check summary "$work/out"

rc=0
/usr/bin/time $tf "$root/target/release/rift" --stats --mem "$mem" \
  "$work/pair/a" "$work/pair/b" > "$work/diff" 2> "$work/err" || rc=$?
[ "$rc" -le 1 ] || { cat "$work/err" >&2; echo "rift failed with exit $rc" >&2; exit 1; }
grep -v '^ ' "$work/err" | grep -E '^(peak heap|time|approx)' || true
echo "diff: $(wc -c < "$work/diff" | tr -d ' ') bytes"
check diff "$work/err"
