#!/bin/sh
# A/B run of two rift builds on one on-disk pair, page cache dropped before
# every timed run, base and head alternating.
#
#   bench/ab-disk.sh [--dry-run] [--keep] BASE HEAD DISKDIR [ROUNDS [ARGS]]
#
#   BASE, HEAD  git refs of this checkout, built once each from `git archive`
#               with cargo build --release --locked -p rift-diff -p rift-gen;
#               or a directory that already holds rift and rift-gen
#   DISKDIR     a directory on the disk under test (not tmpfs); the pair is
#               generated into DISKDIR/rift-ab-pair if it is missing (50 GiB
#               each of A and B, profile sql, seed 7, 0.1% changes, made by
#               BASE's rift-gen) and deleted at the end unless --keep
#   ROUNDS      timed runs per side (default 3)
#   ARGS        what to time, rift arguments before the pair (default
#               "--stats --mem 256M"); no commas
#   --dry-run   check the disk, print the plan, build and run nothing
#
# Each timed run is bench/linux-safe.sh --cold --suite none, so it needs
# sudo -v first, Linux, and everything that script needs. Before the timed
# runs the two builds print the same stdout (sha256, untimed). Result: a
# summary on stdout and rift-ab-disk-<base>-<head>.tar.gz in the current
# directory.
#
# Test knobs: RIFT_AB_SIZE_MIB=N (size of each file, default 51200),
# RIFT_BENCH_TESTING=1 (skip the Linux and tmpfs checks), RIFT_SUDO.
set -eu

die() { echo "ab-disk: $*" >&2; exit 1; }
usage() { sed -n '2,/^set/p' "$0" | sed '$d' >&2; exit 2; }

dry=0 keep=0
while [ $# -gt 0 ]; do
  case $1 in
    --dry-run) dry=1; shift ;;
    --keep) keep=1; shift ;;
    -*) usage ;;
    *) break ;;
  esac
done
[ $# -ge 3 ] && [ $# -le 5 ] || usage
bref=$1 href=$2 disk=$3 rounds=${4:-3} cmd=${5:---stats --mem 256M}
case $rounds in ''|*[!0-9]*) usage ;; esac
[ "$rounds" -ge 1 ] || usage
case $cmd in *,*) die "ARGS must not contain a comma" ;; esac

here=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$here")
safe=$here/linux-safe.sh
sudo_cmd=${RIFT_SUDO:-sudo}
size=${RIFT_AB_SIZE_MIB:-51200}
testing=${RIFT_BENCH_TESTING:-0}
start=$(pwd)

[ -d "$disk" ] && [ -w "$disk" ] || die "$disk is not a writable directory"
disk=$(cd "$disk" && pwd)
[ "$testing" = 1 ] || [ "$(stat -f -c %T "$disk" 2>/dev/null)" != tmpfs ] || die "$disk is on tmpfs; the point is a real disk"
pair=$disk/rift-ab-pair
if [ ! -f "$pair/a" ] || [ ! -f "$pair/b" ]; then
  need=$((2 * size + size / 5))
  free=$(df -Pk "$disk" | awk 'NR==2 {print int($4 / 1024)}')
  [ "$free" -ge "$need" ] || die "$disk has $free MiB free, the pair needs $need MiB (two files of $size MiB plus a fifth as margin)"
fi

# resolve REF: a directory with rift and rift-gen as it is, else a commit.
resolve() {
  if [ -x "$1/rift" ] && [ -x "$1/rift-gen" ]; then (cd "$1" && pwd)
  else git -C "$root" rev-parse --verify --quiet "$1^{commit}" || die "$1 is neither a ref of this checkout nor a directory with rift and rift-gen"
  fi
}
bres=$(resolve "$bref")
hres=$(resolve "$href")
label=$(printf '%.7s' "$(basename "$bres")")-$(printf '%.7s' "$(basename "$hres")")

order=
r=0
while [ "$r" -lt "$rounds" ]; do r=$((r + 1)); order="$order $r:base $r:head"; done

if [ "$dry" = 1 ]; then
  echo "Plan"
  echo "  base            $bres"
  echo "  head            $hres"
  echo "  pair            $pair ($( [ -f "$pair/a" ] && echo present || echo "to generate, $size MiB each"))"
  echo "  timed command   rift $cmd a b, $rounds per side, cache dropped before each"
  echo "  order"
  for o in $order; do
    echo "    ${o%%:*}. ${o#*:}  linux-safe.sh --yes --cold --reps 1 --suite none --bin <${o#*:} build> --read-only-pair <pair> --disk-cmds '$cmd'"
  done
  exit 0
fi

command -v sha256sum >/dev/null 2>&1 || die "sha256sum not found"
$sudo_cmd -v || die "sudo -v failed; --cold needs it"

w=$disk/rift-ab-work
mkdir -m 700 "$w" || die "$w exists; remove it or rerun"
keep_pid=
cleanup() {
  [ -z "$keep_pid" ] || { kill "$keep_pid" 2>/dev/null || true; wait "$keep_pid" 2>/dev/null || true; }
  rm -rf "${w:?}"
  [ "$keep" = 1 ] || rm -rf "${disk:?}/rift-ab-pair"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP

# The builds and the pair take long enough for the credential to expire.
(
  trap 'kill "$s" 2>/dev/null; exit 0' TERM
  trap - EXIT
  while kill -0 "$$" 2>/dev/null; do
    sleep "${RIFT_SUDO_KEEPALIVE:-60}" & s=$!
    wait "$s" || true
    $sudo_cmd -n -v >/dev/null 2>&1 || true
  done
) &
keep_pid=$!

# build NAME RESOLVED: the directory $w/NAME holds rift and rift-gen.
build() {
  mkdir "$w/$1"
  if [ -d "$2" ]; then cp "$2/rift" "$2/rift-gen" "$w/$1/"; return; fi
  echo "Building $1 ($2)" >&2
  mkdir "$w/src-$1"
  git -C "$root" archive "$2" | tar -x -C "$w/src-$1"
  (cd "$w/src-$1" && CARGO_TARGET_DIR=$w/target-$1 nice -n 10 cargo build --release --locked -p rift-diff -p rift-gen) \
    > "$w/build-$1.log" 2>&1 || { tail -n 20 "$w/build-$1.log" >&2; die "build of $1 failed"; }
  cp "$w/target-$1/release/rift" "$w/target-$1/release/rift-gen" "$w/$1/"
  rm -rf "${w:?}/src-$1" "${w:?}/target-$1"
}
build base "$bres"
build head "$hres"

if [ ! -f "$pair/a" ] || [ ! -f "$pair/b" ]; then
  rm -rf "${pair:?}"
  mkdir "$pair"
  echo "Generating the pair, $size MiB each" >&2
  nice -n 10 "$w/base/rift-gen" --seed 7 --size "${size}M" --profile sql --change-ratio 0.001 --out "$pair" > "$w/gen.log" 2>&1 ||
    { tail -n 20 "$w/gen.log" >&2; die "rift-gen failed"; }
fi

# Stdout only; stderr has the timing-dependent stats. $cmd is a word list.
# shellcheck disable=SC2086
outhash() { "$w/$1/rift" $cmd "$pair/a" "$pair/b" 2>/dev/null | sha256sum | cut -d' ' -f1; }
hb=$(outhash base)
hh=$(outhash head)
[ "$hb" = "$hh" ] || die "stdout differs: base $hb, head $hh; nothing was timed"
# The pair differs, so a build that failed prints nothing: two empty outputs are not a match.
[ "$hb" != e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855 ] || die "both builds printed nothing on stdout; nothing was timed"

out=$w/out
mkdir "$out"
for o in $order; do
  d=$out/${o%%:*}-${o#*:}
  mkdir "$d"
  echo "Round ${o%%:*}, ${o#*:}" >&2
  (cd "$d" && "$safe" --yes --cold --reps 1 --suite none --bin "$w/${o#*:}" --read-only-pair "$pair" --disk-cmds "$cmd") \
    > "$d/console.log" 2>&1 || { tail -n 20 "$d/console.log" >&2; die "round ${o%%:*}, ${o#*:} failed"; }
done

# rows.tsv: round, side, wall s, exit, peak heap, load before, load after, governor.
for o in $order; do
  d=$out/${o%%:*}-${o#*:}
  rep=$(tar -xzOf "$d"/rift-bench-*.tar.gz report.md)
  f() { printf '%s\n' "$rep" | sed -n "s/^- $1: //p" | head -n1; }
  printf '%s\n' "$rep" | awk -F'|' -v r="${o%%:*}" -v s="${o#*:}" -v lb="$(f load_before)" -v la="$(f load_after)" -v g="$(f cpu_governor)" '
    function t(x) { gsub(/^ +| +$/, "", x); return x }
    t($3) == "read-only-pair" { printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n", r, s, t($9), t($15), t($13), lb, la, g }' >> "$out/rows.tsv"
done
[ -s "$out/rows.tsv" ] || die "no read-only-pair rows in the reports"
# rift exits 1 when the files differ; anything else is a failed run, not a time.
awk -F'\t' '$4 != 0 && $4 != 1 { bad = 1 } END { exit bad }' "$out/rows.tsv" || die "a timed run failed (exit code in rows.tsv); no summary"

side() { awk -F'\t' -v s="$1" '$2 == s {print $3}' "$out/rows.tsv" | sort -n; }
median() { awk '{v[NR] = $1} END {print NR % 2 ? v[(NR + 1) / 2] : (v[NR / 2] + v[NR / 2 + 1]) / 2}'; }
{
  echo "rift --cold A/B, rift $cmd a b, $rounds per side, alternating, cache dropped before each run"
  echo "base $bres"
  echo "head $hres"
  echo "binaries: base $(sha256sum "$w/base/rift" | cut -d' ' -f1) $("$w/base/rift" --version), head $(sha256sum "$w/head/rift" | cut -d' ' -f1) $("$w/head/rift" --version)"
  echo "stdout sha256 of both builds (untimed): $hb"
  echo
  printf 'round\tside\twall s\texit\tpeak heap\tload before\tload after\tgovernor\n'
  cat "$out/rows.tsv"
  echo
  for s in base head; do
    echo "$s: median $(side "$s" | median) s, min $(side "$s" | sed -n 1p) s, max $(side "$s" | sed -n '$p') s"
  done
  echo "head over base median: $(awk -v b="$(side base | median)" -v h="$(side head | median)" 'BEGIN { printf "%+.1f%%", (h - b) * 100 / b }')"
} > "$out/summary.txt"
cat "$out/summary.txt"

tarball=$start/rift-ab-disk-$label.tar.gz
[ ! -e "$tarball" ] || tarball=$start/rift-ab-disk-$label-$$.tar.gz
tar -C "$w" -czf "$tarball" out
echo "Done: $tarball" >&2
