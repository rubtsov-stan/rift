#!/bin/sh
# CI memory gate: generates a pair, runs `rift --summary --mem MEM`, the
# full diff `rift --stats --mem MEM`, `rift git-diff --stats --mem MEM`
# and, on a shuffled CSV pair, key mode `rift --stats --key csv:1 --mem
# MEM`, and fails when the counting allocator's peak heap of any of them
# exceeds MEM.
#
#   bench/ci-mem.sh [SIZE] [MEM] [small]     defaults 1G and 256M
#
# With `small`, a run may end in a budget refusal, as runs below what the
# pair needs do; its peak heap is checked all the same.
#
# rift ends its "peak heap" line with the exact count, "(N bytes)": on
# stdout for a --summary that finished, on stderr otherwise, so a diff on
# stdout stays a patch. The check compares those bytes with MEM, with no
# rounding.
set -eu

size=${1:-1G} mem=${2:-256M} small=${3:-}
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

# ended NAME RC: fails unless rift exited 0 or 1, or, with `small`, 2 on a
# budget refusal.
ended() {
  [ "$2" -le 1 ] && return
  if [ -n "$small" ] && [ "$2" -eq 2 ] && grep -q ' budget exceeded: ' "$work/err"; then
    echo "$1: refused at --mem $mem"
    return
  fi
  cat "$work/err" >&2
  echo "rift $1 failed with exit $2" >&2
  exit 1
}

# check NAME REPORT...: a REPORT holds rift's "peak heap" line; err holds time's.
check() {
  name=$1; shift
  if [ "$tf" = -l ]; then
    rss=$(awk '/maximum resident set size/ {print $1}' "$work/err")
  else
    rss=$(awk -F': ' '/Maximum resident/ {print $2 * 1024}' "$work/err")
  fi
  peak=$(sed -n 's/^peak heap .*(\([0-9][0-9]*\) bytes)$/\1/p' "$@" | head -n 1)
  [ -n "$peak" ] || { echo "$name: no 'peak heap' line in rift output" >&2; exit 1; }
  echo "$name: peak heap $peak bytes, limit $limit bytes, peak RSS $rss bytes"
  if [ "$peak" -gt "$limit" ]; then
    echo "$name: peak heap exceeds --mem" >&2
    exit 1
  fi
}

rc=0
/usr/bin/time $tf "$root/target/release/rift" --summary --stats --mem "$mem" \
  "$work/pair/a" "$work/pair/b" > "$work/out" 2> "$work/err" || rc=$?
cat "$work/out"
ended summary "$rc"
check summary "$work/out" "$work/err"

rc=0
/usr/bin/time $tf "$root/target/release/rift" --stats --mem "$mem" \
  "$work/pair/a" "$work/pair/b" > "$work/diff" 2> "$work/err" || rc=$?
ended diff "$rc"
grep -v '^ ' "$work/err" | grep -E '^(peak heap|time|approx)' || true
echo "diff: $(wc -c < "$work/diff" | tr -d ' ') bytes"
check diff "$work/err"

rc=0
/usr/bin/time $tf "$root/target/release/rift" git-diff --stats --mem "$mem" \
  f "$work/pair/a" 1111111 100644 "$work/pair/b" 2222222 100644 \
  > /dev/null 2> "$work/err" || rc=$?
ended git-diff "$rc"
check git-diff "$work/err"

# Key mode on a shuffled pair of the same size; its changes go nowhere.
rm -rf "$work/pair" "$work/diff"
"$root/target/release/rift-gen" --seed 7 --size "$size" --profile shuffled --rows csv --out "$work/keyed"
rc=0
/usr/bin/time $tf "$root/target/release/rift" --stats --key csv:1 --mem "$mem" \
  "$work/keyed/a" "$work/keyed/b" > /dev/null 2> "$work/err" || rc=$?
ended key "$rc"
grep -E '^(peak heap|time|passes)' "$work/err" || true
check key "$work/err"
