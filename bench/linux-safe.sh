#!/bin/sh
# Benchmark for a Linux host: no root, nothing written to disk except the
# result archive in the current directory.
#
#   bench/linux-safe.sh [--dry-run] [--yes] [--ref REF] [--repo URL]
#                       [--read-only-pair DIR]
#
#   --dry-run            print the plan and exit
#   --yes                skip the 10 second wait before the run starts
#   --ref REF            tag, branch or commit to build (default v0.1.0)
#   --repo URL           where to clone from (default the public repository)
#   --read-only-pair DIR also run rift and cat over DIR/a and DIR/b, which are
#                        only read (a real-disk number if DIR is on disk)
#
# Test knobs, for running the script on a host that is not Linux. They turn
# safety checks off; never set them for a real run:
#   RIFT_BENCH_TESTING=1   skip the Linux, tmpfs and GNU time checks
#   RIFT_BENCH_MEMINFO=F   read MemAvailable from F instead of /proc/meminfo
#   RIFT_BENCH_SHM=DIR     use DIR instead of /dev/shm
#   RIFT_BENCH_TIME=PATH   use PATH instead of /usr/bin/time
#   RIFT_BENCH_LIMITER=L   force systemd, prlimit or none (plan output only)
#
# See bench/README.md.
set -eu

GIB=1073741824
schema=2
repo=https://github.com/rubtsov-stan/rift ref=v0.1.0 yes=0 dry=0 ropair=
testing=${RIFT_BENCH_TESTING:-0}
meminfo=${RIFT_BENCH_MEMINFO:-/proc/meminfo}
timebin=${RIFT_BENCH_TIME:-/usr/bin/time}
outdir=$(pwd)

die() { echo "linux-safe: $*" >&2; exit 1; }
usage() { sed -n '2,/^set/p' "$0" | sed '$d' >&2; exit 2; }

while [ $# -gt 0 ]; do
  case $1 in
    --dry-run) dry=1; shift ;;
    --yes) yes=1; shift ;;
    --ref) [ $# -ge 2 ] || usage; ref=$2; shift 2 ;;
    --repo) [ $# -ge 2 ] || usage; repo=$2; shift 2 ;;
    --read-only-pair) [ $# -ge 2 ] || usage; ropair=$2; shift 2 ;;
    *) usage ;;
  esac
done

# ---- checks that need no scratch directory ---------------------------------

[ "$(id -u)" -ne 0 ] || die "refusing to run as root; nothing here needs it"
if [ "$testing" != 1 ]; then
  [ "$(uname -s)" = Linux ] || die "Linux only"
  "$timebin" -v true >/dev/null 2>&1 || die "GNU time not found at $timebin (package: time)"
fi
command -v cargo >/dev/null 2>&1 ||
  die "cargo not found. Install a Rust toolchain with rustup first; this script does not install one."
command -v git >/dev/null 2>&1 || die "git not found"
if [ -n "$ropair" ]; then
  [ -f "$ropair/a" ] && [ -f "$ropair/b" ] || die "$ropair must hold files a and b"
fi

is_tmpfs() { [ "$testing" = 1 ] || [ "$(stat -f -c %T "$1" 2>/dev/null)" = tmpfs ]; }
free_bytes() { df -Pk "$1" | awk 'NR==2 {print $4 * 1024}'; }

# ---- scratch location: tmpfs only ------------------------------------------

base=
for d in "${RIFT_BENCH_SHM:-/dev/shm}" "${XDG_RUNTIME_DIR:-}"; do
  if [ -n "$d" ] && [ -d "$d" ] && [ -w "$d" ] && is_tmpfs "$d" &&
     [ "$(free_bytes "$d")" -ge $((3 * GIB)) ]; then
    base=$d
    break
  fi
done
[ -n "$base" ] || die "no writable tmpfs with 3 GiB free (/dev/shm, \$XDG_RUNTIME_DIR); refusing to touch the disk"
tmpfs_free=$(free_bytes "$base")

# ---- sizing ----------------------------------------------------------------

avail=$(awk '/^MemAvailable:/ {print $2 * 1024}' "$meminfo" 2>/dev/null || true)
[ -n "$avail" ] || die "no MemAvailable in $meminfo"
limit=$((avail * 60 / 100))
mem_total=$(awk '/^MemTotal:/ {print $2 * 1024}' "$meminfo")

# fits SIZE CAP: SIZE bytes of A and of B in tmpfs, the cap of the measured
# command and a 2 GiB margin stay within 60% of MemAvailable, and the pair
# fits the tmpfs with 1 GiB to spare for the build.
fits() {
  [ $((2 * $1 + $2 + 2 * GIB)) -le "$limit" ] && [ $((2 * $1 + GIB)) -le "$tmpfs_free" ]
}

maxg=0
for g in 1 2 4 8 16; do
  if fits $((g * GIB)) "$GIB"; then maxg=$g; fi
done
[ "$maxg" -gt 0 ] || die "refusing to start: a 1 GiB pair needs $(( (2 * GIB + GIB + 2 * GIB) * 100 / 60 / 1048576 )) MiB of MemAvailable (60% rule), this host has $((avail / 1048576)) MiB"

sizes=$GIB
[ "$maxg" -gt 1 ] && sizes="$sizes $((maxg * GIB))"
diffcap=$((3 * GIB))
if fits "$GIB" "$diffcap"; then diffok=1; else diffok=0; fi

# ---- memory limiter --------------------------------------------------------

limiter=none
if command -v systemd-run >/dev/null 2>&1 &&
   systemd-run --user --scope -q -p MemoryMax=64M true >/dev/null 2>&1; then
  limiter=systemd
elif command -v prlimit >/dev/null 2>&1; then
  limiter=prlimit
fi
[ "$testing" != 1 ] || limiter=${RIFT_BENCH_LIMITER:-$limiter}
niceio="nice -n 10"
command -v ionice >/dev/null 2>&1 && niceio="$niceio ionice -c3"
[ "$limiter" = none ] && diffok=0
cmp=$((GIB / 4)) cmpcap=$((2 * GIB))
if [ "$limiter" != none ] && fits "$cmp" "$cmpcap"; then cmpok=1; else cmpok=0; fi

cpus=$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN)
have_taskset=0
command -v taskset >/dev/null 2>&1 && have_taskset=1

# The run is labelled by CPU model and date, never by hostname, so the
# archive and the report can be shared as they are.
cpu_model=$(lscpu 2>/dev/null | awk -F': +' '$1 == "Model name" {print $2; exit}')
label=linux-$(printf '%s' "${cpu_model:-unknown cpu}" | tr '[:upper:]' '[:lower:]' |
  sed 's/([a-z]*)//g; s/ w\/ .*//; s/ with .*//; s/ @.*//; s/ cpu$//; s/ processor$//; s/[^a-z0-9][^a-z0-9]*/-/g; s/^-//' |
  cut -c1-40 | sed 's/-$//')-$(date +%Y-%m-%d)

gib() { awk -v b="$1" 'BEGIN { if (b < 1073741824) printf "%d MiB", b / 1048576; else printf "%.1f GiB", b / 1073741824 }'; }

plan() {
  echo "Plan"
  echo "  MemAvailable      $(gib "$avail") of $(gib "$mem_total"); 60% budget $(gib "$limit")"
  echo "  scratch           tmpfs $base ($(gib "$tmpfs_free") free)"
  echo "  source            $repo at $ref"
  echo "  pair sizes        $(for s in $sizes; do printf '%s ' "$(gib "$s")"; done)(A and B each), profiles sql and csv, seed 7, 0.1% changes"
  echo "  tmpfs peak        $(gib $((2 * ${sizes##* })))"
  case $limiter in
    systemd) echo "  memory cap        systemd-run --user scope, MemoryMax, MemorySwapMax=0: rift 1 GiB (1.5 GiB for --mem 1G)" ;;
    prlimit) echo "  memory cap        prlimit --as (systemd user scope unavailable): address space, not RSS" ;;
    none) echo "  memory cap        none available (no systemd user scope, no prlimit): diff and git diff skipped" ;;
  esac
  if [ "$diffok" = 1 ]; then
    echo "  diff, git diff    run on the $(gib "$GIB") pair, cap $(gib "$diffcap")"
  else
    echo "  diff, git diff    skipped: a $(gib "$diffcap") cap does not fit the 60% budget or no limiter"
  fi
  if [ "$cmpok" = 1 ]; then
    echo "  comparison        rift, diff and git diff on a $(gib "$cmp") pair, cap $(gib "$cmpcap")"
  else
    echo "  comparison        skipped: a $(gib "$cmpcap") cap does not fit the 60% budget or no limiter"
  fi
  if [ "$have_taskset" = 1 ]; then
    echo "  thread scaling    taskset -c 0..k-1 for k = 1 2 4 and all $cpus CPUs"
  else
    echo "  thread scaling    skipped: taskset not found"
  fi
  [ -z "$ropair" ] || echo "  read-only pair    $ropair (only read)"
  echo "  output            rift-bench-$label.tar.gz in the current directory"
}

plan
[ "$dry" = 0 ] || exit 0

if [ "$yes" != 1 ]; then
  echo "Starting in 10 s. Ctrl-C to cancel." >&2
  sleep 10
fi

# ---- scratch directory and cleanup -----------------------------------------

scratch=$base/rift-bench-$$
cleanup() {
  case $scratch in
    */rift-bench-*) rm -rf "$scratch" ;;
  esac
}
# Arm the cleanup only once the directory is ours: if it already exists,
# mkdir fails and the existing directory is left alone.
mkdir -m 700 "$scratch" || die "$scratch already exists; remove it or rerun"
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

raw=$scratch/pkg/raw
mkdir -p "$raw" "$scratch/bin"
runs=$scratch/runs.tsv notes=$scratch/notes.txt facts=$scratch/facts.tsv killed=$scratch/killed.txt
: > "$runs"; : > "$notes"; : > "$facts"; : > "$killed"

fact() { printf '%s\t%s\n' "$1" "$2" >> "$facts"; }
note() { echo "$*" >> "$notes"; echo "note: $*" >&2; }
loadavg() { cut -d' ' -f1-3 /proc/loadavg 2>/dev/null || echo n/a; }

# ---- build -----------------------------------------------------------------

git clone -q "$repo" "$scratch/src" || die "clone of $repo failed"
git -C "$scratch/src" checkout -q "$ref" || die "ref $ref not found in $repo"
commit=$(git -C "$scratch/src" rev-parse HEAD)

# Everything cargo writes goes to the scratch directory. Keep rustup's
# toolchains where they are and forbid installing one.
RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup}
export CARGO_HOME="$scratch/cargo" CARGO_TARGET_DIR="$scratch/target" RUSTUP_HOME RUSTUP_AUTO_INSTALL=0
locked=
[ -f "$scratch/src/Cargo.lock" ] && locked=--locked
rustc_v=$(cd "$scratch/src" && rustc -V) || die "no usable rustc for $ref (a pinned toolchain that is not installed is never fetched)"
echo "Building $ref ($commit) with $rustc_v" >&2
# shellcheck disable=SC2086
(cd "$scratch/src" && $niceio cargo build --release $locked -p rift-diff -p rift-gen) >"$raw/build.log" 2>&1 ||
  { tail -n 20 "$raw/build.log" >&2; die "build failed"; }
cp "$scratch/target/release/rift" "$scratch/target/release/rift-gen" "$scratch/bin/"
rm -rf "$scratch/target" "$scratch/cargo" "$scratch/src"
rift=$scratch/bin/rift gen=$scratch/bin/rift-gen

# ---- host facts ------------------------------------------------------------

lscpu_get() { lscpu 2>/dev/null | awk -F': +' -v k="$1" '$1 == k {print $2; exit}'; }
fact tool "bench/linux-safe.sh schema $schema"
fact note "Files sit in tmpfs (RAM): this run measures CPU, memory and the algorithm, not the SSD."
fact host "$label"
fact date "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
fact cpu_model "$cpu_model"
fact cpu_threads "$cpus"
fact cpu_cores_per_socket "$(lscpu_get 'Core(s) per socket')"
fact cpu_sockets "$(lscpu_get 'Socket(s)')"
fact cpu_governor "$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo unreadable)"
fact mem_total_bytes "$mem_total"
fact mem_available_bytes "$avail"
fact kernel "$(uname -r)"
# shellcheck disable=SC1091
fact distro "$(. /etc/os-release 2>/dev/null && echo "${PRETTY_NAME:-unknown}" || echo unknown)"
fact virt "$(systemd-detect-virt 2>/dev/null || echo unknown)"
fact rustc "$rustc_v"
fact rift_ref "$ref"
fact rift_commit "$commit"
fact scratch_fs "tmpfs, $(gib "$tmpfs_free") free of the mount at start"
fact limiter "$limiter"
fact load_before "$(loadavg)"

# ---- measured runs ---------------------------------------------------------

n=0

# bytes of "peak heap 9.8 MiB of ..." in FILE
heap_bytes() {
  awk '/^peak heap/ {
    m = 1; if ($4 == "KiB") m = 1024; else if ($4 == "MiB") m = 1048576
    else if ($4 == "GiB") m = 1073741824; else if ($4 == "TiB") m = 1099511627776
    printf "%.0f", $3 * m; exit }' "$1" 2>/dev/null || true
}

# run_one GROUP PROFILE SIZE TOOL THREADS CAP REP MODE CMD...
# MODE: count (stdout counted by wc -c), file
# (stdout kept, for rift --summary). time -v writes to its own file, so the
# command's stderr stays clean for rift's --stats line.
# scope_result UNIT: systemd's Result for the scope (success, oom-kill,
# exit-code, ...), read without root. A failed scope stays loaded until
# reset-failed, which also keeps the user manager tidy; a clean one is
# collected and reads as success. n/a without a systemd scope.
scope_result() {
  [ -n "$1" ] || { echo n/a; return; }
  sw=0
  while [ "$sw" -lt 50 ]; do
    case $(systemctl --user show "$1.scope" -p ActiveState --value 2>/dev/null) in
      active|activating|deactivating) sleep 0.1; sw=$((sw + 1)) ;;
      *) break ;;
    esac
  done
  sr=$(systemctl --user show "$1.scope" -p Result --value 2>/dev/null || true)
  systemctl --user reset-failed "$1.scope" >/dev/null 2>&1 || true
  echo "${sr:-unknown}"
}

run_one() {
  grp=$1 prof=$2 sz=$3 tool=$4 thr=$5 cap=$6 rep=$7 mode=$8; shift 8
  n=$((n + 1))
  id=$(printf '%03d' "$n")
  b=$raw/$id
  # Once the cap kills a command on a pair, its later repetitions would only
  # be killed again: record them as skipped instead.
  key="$grp|$prof|$sz|$tool|$thr"
  if grep -qxF "$key" "$killed"; then
    echo "[$id] $grp $prof $(gib "$sz") $tool threads=$thr rep=$rep: skipped, killed by its cap before" >&2
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t\t\t\t\t\t\t\tskipped\t%s\n' \
      "$id" "$grp" "$prof" "$sz" "$tool" "$thr" "$cap" "$rep" "$*" >> "$runs"
    return
  fi
  echo "[$id] $grp $prof $(gib "$sz") $tool threads=$thr rep=$rep" >&2
  unit=
  case $limiter in
    systemd)
      unit=rift-bench-$$-$id
      lim="systemd-run --user --scope -q --unit=$unit -p MemoryMax=$cap -p MemorySwapMax=0 $niceio" ;;
    prlimit) lim="prlimit --as=$cap $niceio" ;;
    *) lim=$niceio ;;
  esac
  # $lim is a word list on purpose.
  # shellcheck disable=SC2086
  case $mode in
    file) $lim "$timebin" -v -o "$b.time" "$@" > "$b.out" 2> "$b.err" || true ;;
    count) $lim "$timebin" -v -o "$b.time" "$@" 2> "$b.err" | wc -c | tr -d ' ' > "$b.bytes" || true ;;
  esac
  ec=$(awk -F': ' '/Exit status/ {print $2}' "$b.time" 2>/dev/null || true)
  wall=$(awk -F': ' '/Elapsed/ {n = split($NF, t, ":"); s = 0; for (i = 1; i <= n; i++) s = s * 60 + t[i]; print s}' "$b.time" 2>/dev/null || true)
  user=$(awk -F': ' '/User time/ {print $2}' "$b.time" 2>/dev/null || true)
  sys=$(awk -F': ' '/System time/ {print $2}' "$b.time" 2>/dev/null || true)
  rss=$(awk -F': ' '/Maximum resident/ {print $2}' "$b.time" 2>/dev/null || true)
  result=$(scope_result "$unit")
  [ "$result" != oom-kill ] || echo "$key" >> "$killed"
  heap=
  case $tool in
    *--summary*) heap=$(heap_bytes "$b.out") ;;
    *--stats*) heap=$(heap_bytes "$b.err") ;;
  esac
  case $mode in
    count) out=$(cat "$b.bytes") ;;
    file) out=$(wc -c < "$b.out" | tr -d ' ') ;;
    *) out= ;;
  esac
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$id" "$grp" "$prof" "$sz" "$tool" "$thr" "$cap" "$rep" "$wall" "$user" "$sys" "$rss" "$ec" "$heap" "$out" "$result" "$*" >> "$runs"
}

# core_runs GROUP PROFILE SIZE A B: the runs every pair gets.
core_runs() {
  g=$1 p=$2 s=$3 a=$4 bb=$5
  for r in 1 2 3; do
    # Into a pipe, not /dev/null: cat can hand a file to /dev/null without
    # copying it, which made the baseline read 2 GiB in 0.05 s.
    run_one "$g" "$p" "$s" "cat a b | wc -c" all "$GIB" $r count cat "$a" "$bb"
  done
  for r in 1 2 3; do
    run_one "$g" "$p" "$s" "rift --summary --mem 256M" all "$GIB" $r file "$rift" --summary --mem 256M "$a" "$bb"
  done
  for r in 1 2 3; do
    run_one "$g" "$p" "$s" "rift --stats --mem 256M" all "$GIB" $r count "$rift" --stats --mem 256M "$a" "$bb"
  done
  run_one "$g" "$p" "$s" "rift --stats --mem 64M" all "$GIB" 1 count "$rift" --stats --mem 64M "$a" "$bb"
  # 1 GiB of heap does not fit a 1 GiB cap; the extra 0.5 GiB comes out of the 2 GiB margin.
  run_one "$g" "$p" "$s" "rift --stats --mem 1G" all $((GIB + GIB / 2)) 1 count "$rift" --stats --mem 1G "$a" "$bb"
}

# The log lives in the scratch directory, which the exit trap removes.
gen_failed() { rc=$1; tail -n 20 "$2" >&2; die "rift-gen failed (exit $rc)"; }

small=${sizes%% *}
pair=$scratch/pair

# gen_pair SIZE PROFILE: generate the pair into $pair.
gen_pair() {
  echo "Generating $2 $(gib "$1")" >&2
  rm -rf "$pair"
  glog=$raw/gen-$2-$1.log
  # shellcheck disable=SC2086
  if [ "$limiter" = systemd ]; then
    # tmpfs pages count against the cgroup of the process that writes
    # them, so the cap covers the pair rift-gen writes plus its own 1 GiB.
    systemd-run --user --scope -q -p MemoryMax=$((2 * $1 + GIB)) -p MemorySwapMax=0 $niceio \
      "$gen" --seed 7 --size "$1" --profile "$2" --change-ratio 0.001 --out "$pair" > "$glog" 2>&1 || gen_failed $? "$glog"
  else
    $niceio "$gen" --seed 7 --size "$1" --profile "$2" --change-ratio 0.001 --out "$pair" > "$glog" 2>&1 || gen_failed $? "$glog"
  fi
  cp "$pair/manifest.tsv" "$raw/manifest-$2-$1.tsv" 2>/dev/null || true
}

# A pair small enough for diff and git diff to fit their cap, so all three
# tools get real times on the same data.
if [ "$cmpok" = 1 ]; then
  for prof in sql csv; do
    gen_pair "$cmp" "$prof"
    for r in 1 2 3; do
      run_one compare "$prof" "$cmp" "rift --stats --mem 256M" all "$GIB" $r count "$rift" --stats --mem 256M "$pair/a" "$pair/b"
      run_one compare "$prof" "$cmp" "diff a b" all "$cmpcap" $r count diff "$pair/a" "$pair/b"
      run_one compare "$prof" "$cmp" "git diff --no-index a b" all "$cmpcap" $r count git diff --no-index "$pair/a" "$pair/b"
    done
    rm -rf "$pair"
  done
else
  note "comparison on the $(gib "$cmp") pair not run: no limiter, or a $(gib "$cmpcap") cap does not fit within 60% of MemAvailable"
fi
for size in $sizes; do
  for prof in sql csv; do
    gen_pair "$size" "$prof"
    core_runs generated "$prof" "$size" "$pair/a" "$pair/b"

    if [ "$size" = "$small" ]; then
      for r in 1 2 3; do
        if [ "$diffok" = 1 ]; then
          run_one generated "$prof" "$size" "diff a b" all "$diffcap" $r count diff "$pair/a" "$pair/b"
          run_one generated "$prof" "$size" "git diff --no-index a b" all "$diffcap" $r count git diff --no-index "$pair/a" "$pair/b"
        fi
      done
      [ "$diffok" = 1 ] || note "diff and git diff not run on $prof $(gib "$size"): no limiter, or a $(gib "$diffcap") cap does not fit within 60% of MemAvailable"
      # rift sizes its threads from available_parallelism, which follows the
      # CPU affinity mask, so taskset is what changes the thread count.
      if [ "$have_taskset" = 1 ]; then
        for k in 1 2 4 "$cpus"; do
          [ "$k" -le "$cpus" ] || continue
          if [ "$k" -eq "$cpus" ]; then pre=; thr=all; else pre="taskset -c 0-$((k - 1))"; thr=$k; fi
          for r in 1 2 3; do
            # shellcheck disable=SC2086
            run_one scaling "$prof" "$size" "rift --summary --mem 256M" "$thr" "$GIB" $r file $pre "$rift" --summary --mem 256M "$pair/a" "$pair/b"
          done
        done
      else
        note "thread scaling not run: taskset not found"
      fi
    fi
    rm -rf "$pair"
  done
done

if [ -n "$ropair" ]; then
  rsz=$(wc -c < "$ropair/a" | tr -d ' ')
  core_runs read-only-pair existing "$rsz" "$ropair/a" "$ropair/b"
fi

fact load_after "$(loadavg)"
[ "$limiter" != prlimit ] || note "no systemd user scope: caps are prlimit --as address-space limits, which are stricter than RSS caps"

# ---- report ----------------------------------------------------------------

pkg=$scratch/pkg
# Input lines are tagged F (fact), N (note) or R (run) so one awk can read all three.
{ awk '{print "F\t" $0}' "$facts"; awk '{print "N\t" $0}' "$notes"; awk '{print "R\t" $0}' "$runs"; } | awk -F'\t' -v schema="$schema" '
function esc(s) { gsub(/\\/, "\\\\", s); gsub(/"/, "\\\"", s); gsub(/[\001-\037]/, " ", s); return s }
function num(x) { return x == "" ? "null" : x }
$1 == "F" { f[++nf] = $2; v[nf] = $3; next }
$1 == "N" { nt[++nn] = $2; next }
{
  r[++nr] = sprintf("    {\"id\": \"%s\", \"group\": \"%s\", \"profile\": \"%s\", \"size_bytes\": %s, \"tool\": \"%s\", \"threads\": \"%s\", \"cap_bytes\": %s, \"rep\": %s, \"wall_s\": %s, \"user_s\": %s, \"sys_s\": %s, \"max_rss_kb\": %s, \"exit_code\": %s, \"rift_peak_heap_bytes\": %s, \"output_bytes\": %s, \"result\": \"%s\", \"oom_killed\": %s, \"command\": \"%s\"}",
    $2, $3, $4, num($5), esc($6), $7, num($8), num($9), num($10), num($11), num($12), num($13), num($14), num($15), num($16), $17, ($17 == "oom-kill" ? "true" : "false"), esc($18))
}
END {
  print "{\n  \"schema_version\": " schema ",\n  \"host\": {"
  for (i = 1; i <= nf; i++) printf "    \"%s\": \"%s\"%s\n", f[i], esc(v[i]), (i < nf ? "," : "")
  print "  },\n  \"notes\": ["
  for (i = 1; i <= nn; i++) printf "    \"%s\"%s\n", esc(nt[i]), (i < nn ? "," : "")
  print "  ],\n  \"runs\": ["
  for (i = 1; i <= nr; i++) print r[i] (i < nr ? "," : "")
  print "  ]\n}"
}' > "$pkg/results.json"

{
  echo "Files sit in tmpfs (RAM): this run measures CPU, memory and the algorithm, not the SSD."
  echo
  echo "## Host"
  echo
  awk -F'\t' '$1 != "note" {printf "- %s: %s\n", $1, $2}' "$facts"
  echo
  echo "## Plan"
  echo
  echo '```'
  plan
  echo '```'
  if [ -s "$notes" ]; then
    echo
    echo "## Notes"
    echo
    sed 's/^/- /' "$notes"
  fi
  echo
  echo "## Runs"
  echo
  echo "wall, user and sys in seconds; peak heap is rift's own count; result is systemd's verdict on the run's scope (oom-kill means the cap killed it; skipped means an earlier repetition was killed, so this one was not run; n/a without systemd)."
  echo
  echo "| id | group | profile | size | tool | threads | rep | wall | user | sys | max RSS | peak heap | output | exit | result |"
  echo "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
  awk -F'\t' '
  function h(b,   u, i) { if (b == "") return ""; split("B KiB MiB GiB TiB", u, " "); i = 1; while (b >= 1024 && i < 5) { b /= 1024; i++ } return sprintf(i == 1 ? "%d %s" : "%.1f %s", b, u[i]) }
  { printf "| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |\n",
      $1, $2, $3, h($4), $5, $6, $8, $9, $10, $11, h($12 == "" ? "" : $12 * 1024), h($14), h($15), $13, $16 }' "$runs"
} > "$pkg/report.md"

name=rift-bench-$label
archive=$outdir/$name.tar.gz
[ ! -e "$archive" ] || archive=$outdir/$name-$$.tar.gz
tar -C "$pkg" -czf "$archive" report.md results.json raw
echo "Done: $archive" >&2
