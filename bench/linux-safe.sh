#!/bin/sh
# Benchmark for a Linux host: no root, nothing written to disk except the
# result archive in the current directory, unless --disk-dir is given.
#
#   bench/linux-safe.sh [--dry-run] [--yes] [--ref REF] [--repo URL]
#                       [--read-only-pair DIR] [--disk-dir DIR]
#                       [--suite standard|le|tools|all] [--reps N] [--cold]
#
#   --dry-run            print the plan and exit
#   --yes                skip the 10 second wait before the run starts
#   --ref REF            tag, branch or commit to build (default v0.2.0)
#   --repo URL           where to clone from (default the public repository)
#   --read-only-pair DIR also run rift and cat over DIR/a and DIR/b, which are
#                        only read (a real-disk number if DIR is on disk)
#   --disk-dir DIR       also run a 50 GiB pair (A and B 50 GiB each) generated
#                        into DIR, which must be on a real disk with 110 GiB
#                        free; the pair is deleted afterwards
#   --suite S            standard (default): the sql and csv runs; le: the
#                        low-entropy runs, which need a REF whose rift-gen
#                        has the distinct profile; all: both; tools: rift
#                        against other diff and CSV tools (difftastic,
#                        DuckDB, csvdiff, qsv, daff, each if installed) on
#                        256 MiB, 1 GiB and, with --disk-dir, 4 GiB pairs
#   --reps N             repetitions of each repeated run, 1 to 20 (default 3)
#   --cold               the one option that uses root: before each timed run
#                        of an on-disk group (--disk-dir, --read-only-pair)
#                        drop the page cache, and hold every CPU's governor at
#                        performance for the whole run, restoring each CPU's own
#                        afterwards. Uses sudo -n only, refreshed every 60 s
#                        while the script runs: run sudo -v first.
#
# Test knobs, for running the script on a host that is not Linux. They turn
# safety checks off; never set them for a real run:
#   RIFT_BENCH_TESTING=1   skip the Linux, tmpfs and GNU time checks
#   RIFT_BENCH_MEMINFO=F   read MemAvailable from F instead of /proc/meminfo
#   RIFT_BENCH_SHM=DIR     use DIR instead of /dev/shm
#   RIFT_BENCH_TIME=PATH   use PATH instead of /usr/bin/time
#   RIFT_BENCH_LIMITER=L   force systemd, prlimit or none (plan output only)
#   RIFT_BENCH_DISK_SIZE=N size in bytes of each file of the --disk-dir pair
#                          (default 50 GiB); the disk size and tmpfs checks are
#                          the only ones it relaxes: free space is still checked
#   RIFT_BENCH_LE_SIZES=S  low-entropy pair sizes in bytes, space-separated
#   RIFT_BENCH_TOOLS_SIZES=S  tools-suite pair sizes in bytes, space-separated
#   RIFT_BENCH_TOOLS_TIMEOUT=N  seconds before a tools-suite run is stopped
#   RIFT_BENCH_TOOLS_TIMELINE=N pair size in bytes for the RSS timelines
#   RIFT_SUDO=CMD          use CMD instead of sudo for --cold
#   RIFT_SUDO_KEEPALIVE=N  seconds between the sudo -n -v refreshes (default 60)
#   RIFT_SYSFS_CPU=DIR     use DIR instead of /sys/devices/system/cpu
#   RIFT_DROP_CACHES=F     write to F instead of /proc/sys/vm/drop_caches
#
# See bench/README.md.
set -eu

GIB=1073741824
schema=4
repo=https://github.com/rubtsov-stan/rift ref=v0.2.0 yes=0 dry=0 suite=standard reps=3 ropair='' cold=0
diskdir=
diskroot=
testing=${RIFT_BENCH_TESTING:-0}
meminfo=${RIFT_BENCH_MEMINFO:-/proc/meminfo}
timebin=${RIFT_BENCH_TIME:-/usr/bin/time}
outdir=$(pwd)

die() { echo "linux-safe: $*" >&2; exit 1; }
usage() { sed -n '2,/^set/p' "$0" | sed '$d' >&2; exit 2; }

# cold:begin
# --cold: page cache drops and the performance governor, through sudo -n
# only, so a missing credential fails at the start, not mid-run.
sudo_cmd=${RIFT_SUDO:-sudo}
sysfs_cpu=${RIFT_SYSFS_CPU:-/sys/devices/system/cpu}
drop_file=${RIFT_DROP_CACHES:-/proc/sys/vm/drop_caches}
cold_saved='' cold_before='' cold_failed='' cold_skipped='' cold_keep_pid='' cold_drops=0
cold_nl='
'
# `sh -c :` is what the drops and writes need, so a sudoers that allows only
# some commands fails here and not after the build. Then one write to
# drop_caches and each governor written back to itself prove the files take
# writes, so a read-only /proc/sys or sysfs fails here and not after the runs.
cold_check() {
  $sudo_cmd -n sh -c : >/dev/null 2>&1 || die "--cold needs sudo without a prompt for sh: run sudo -v first, then rerun"
  cold_write 3 "$drop_file" 2>/dev/null || die "--cold cannot write drop_caches ($drop_file); nothing was changed"
  for f in "$sysfs_cpu"/cpu[0-9]*/cpufreq/scaling_governor; do
    [ -e "$f" ] || continue
    g=$(cat "$f" 2>/dev/null) && [ -n "$g" ] || continue
    cold_write "$g" "$f" 2>/dev/null || die "--cold cannot write $f; nothing was changed"
  done
}
# An on-disk group runs for --read-only-pair, and for --disk-dir except in the
# tools suite, whose disk rows are in group tools.
cold_has_disk_group() { [ -n "$ropair" ] || { [ -n "$diskdir" ] && [ "$suite" != tools ]; }; }
# What the report says about the drops is the count that happened.
cold_cache_desc() {
  if [ "$cold_drops" = 0 ]; then
    echo "no on-disk group ran, no drop"
  else
    echo "dropped $cold_drops times, before each timed run of the on-disk groups (sync, then echo 3 > drop_caches); not before tmpfs runs"
  fi
}
# cold_ram_clause PAIR_BYTES MEM_TOTAL: the RAM remark of the on-disk note.
cold_ram_clause() {
  if [ "$1" -gt "$2" ]; then
    echo "; that exceeds RAM, so most reads come from the disk."
  elif [ "$cold" = 1 ]; then
    echo "; that fits in RAM, so a later pass within the run can read the pair from the page cache (the drop is only before each run): these are not fully cold numbers."
  else
    echo "; that fits in RAM, so repeated reads may come from the page cache and these are not cold-disk numbers."
  fi
}
# Keeps the credential alive for the whole run. A refresh that fails is not
# an error here: the next real sudo -n fails the run. The loop stops when its
# parent is gone, so a SIGKILL cannot leave it behind.
cold_keepalive() {
  (
    trap 'kill "$s" 2>/dev/null; exit 0' TERM
    trap - EXIT
    while kill -0 "$$" 2>/dev/null; do
      sleep "${RIFT_SUDO_KEEPALIVE:-60}" & s=$!
      wait "$s" || true
      $sudo_cmd -n -v >/dev/null 2>&1 || true
    done
  ) &
  cold_keep_pid=$!
}
cold_keep_stop() {
  [ -z "$cold_keep_pid" ] || { kill "$cold_keep_pid" 2>/dev/null || true; wait "$cold_keep_pid" 2>/dev/null || true; }
  cold_keep_pid=''
}
# Only runs that read a pair from disk are dropped. Dropping before a tmpfs
# run would evict the compared tools' binaries but not rift's, which sits in
# tmpfs, and add a sudo call per run for nothing.
cold_run_drop() {
  [ "$cold" = 1 ] || return 0
  case $1 in disk|read-only-pair) cold_drop ;; esac
}
cold_drop() {
  sync
  # shellcheck disable=SC2016
  $sudo_cmd -n sh -c 'echo 3 > "$1"' sh "$drop_file" || die "could not drop the page cache (sudo -n failed; run sudo -v again)"
  cold_drops=$((cold_drops + 1))
}
# cold_write VALUE FILE
# shellcheck disable=SC2016
cold_write() { $sudo_cmd -n sh -c 'echo "$1" > "$2"' sh "$1" "$2"; }
# cold_cpu PATH: cpuN from a path under that CPU's cpufreq directory.
cold_cpu() { c=${1%/cpufreq/*}; echo "${c##*/}"; }
# Saves "VALUE PATH" lines for each CPU's governor, then its
# energy_performance_preference when the file exists (intel_pstate and
# amd_pstate change it with the governor), before writing the governor. A CPU
# whose governor cannot be read is skipped and listed in $cold_skipped. A host
# without cpufreq saves nothing.
cold_set() {
  for f in "$sysfs_cpu"/cpu[0-9]*/cpufreq/scaling_governor; do
    [ -e "$f" ] || continue
    g=$(cat "$f" 2>/dev/null) && [ -n "$g" ] || { cold_skipped="$cold_skipped$(cold_cpu "$f") "; continue; }
    cold_saved="$cold_saved$g $f
"
    e=${f%scaling_governor}energy_performance_preference
    if [ -e "$e" ] && v=$(cat "$e" 2>/dev/null) && [ -n "$v" ]; then
      cold_saved="$cold_saved$v $e
"
    fi
    cold_write performance "$f" || die "could not set the governor of $f (sudo -n failed)"
  done
}
# Writes back in saved order (governor, then its preference). An entry leaves
# $cold_saved only after its write was attempted, so a signal in the middle
# lets the exit trap finish the rest. A write that fails is listed in
# $cold_failed with what the file holds now. Safe to call twice; runs from the
# exit trap, so it never dies.
cold_restore() {
  [ -n "$cold_saved" ] || return 0
  [ -n "$cold_before" ] || cold_before=$cold_saved
  while [ -n "$cold_saved" ]; do
    line=${cold_saved%%"$cold_nl"*}
    g=${line%% *} p=${line#* }
    if ! cold_write "$g" "$p" 2>/dev/null; then
      now=$(cat "$p" 2>/dev/null || echo unreadable)
      cold_failed="$cold_failed$(cold_cpu "$p") ${p##*/} left on $now (wanted $g); "
      echo "linux-safe: could not restore $p to $g" >&2
    fi
    cold_saved=${cold_saved#"$line$cold_nl"}
  done
}
# cold_vals KIND [now]: the distinct values saved for scaling_governor or
# energy_performance_preference, or those the files hold now.
cold_vals() {
  printf '%s' "$cold_before$cold_saved" | while read -r g p; do
    case $p in */"$1") if [ "${2:-}" = now ]; then cat "$p"; else echo "$g"; fi ;; esac
  done | sort -u | tr '\n' ' ' | sed 's/ $//'
}
# The governor of cpu0 as the runs see it: read after cold_set.
cold_host_governor() { cat "$sysfs_cpu/cpu0/cpufreq/scaling_governor" 2>/dev/null || echo unreadable; }
# cold:end

while [ $# -gt 0 ]; do
  case $1 in
    --dry-run) dry=1; shift ;;
    --yes) yes=1; shift ;;
    --ref) [ $# -ge 2 ] || usage; ref=$2; shift 2 ;;
    --repo) [ $# -ge 2 ] || usage; repo=$2; shift 2 ;;
    --read-only-pair) [ $# -ge 2 ] || usage; ropair=$2; shift 2 ;;
    --disk-dir) [ $# -ge 2 ] || usage; diskdir=$2; shift 2 ;;
    --suite) [ $# -ge 2 ] || usage; suite=$2; shift 2 ;;
    --reps) [ $# -ge 2 ] || usage; reps=$2; shift 2 ;;
    --cold) cold=1; shift ;;
    *) usage ;;
  esac
done

# ---- checks that need no scratch directory ---------------------------------

[ "$(id -u)" -ne 0 ] || die "refusing to run as root; nothing here needs it"
if [ "$testing" != 1 ]; then
  [ "$(uname -s)" = Linux ] || die "Linux only"
  "$timebin" -v true >/dev/null 2>&1 || die "GNU time not found at $timebin (package: time)"
fi
[ "$cold" = 0 ] || [ "$dry" = 1 ] || cold_check
command -v cargo >/dev/null 2>&1 ||
  die "cargo not found. Install a Rust toolchain with rustup first; this script does not install one."
command -v git >/dev/null 2>&1 || die "git not found"
if [ -n "$ropair" ]; then
  [ -f "$ropair/a" ] && [ -f "$ropair/b" ] || die "$ropair must hold files a and b"
fi
case $reps in
  ''|*[!0-9]*) usage ;;
esac
[ "$reps" -ge 1 ] && [ "$reps" -le 20 ] || usage
case $suite in
  standard) ;;
  tools)
    for t in timeout cmp sort comm; do
      command -v "$t" >/dev/null 2>&1 || die "$t not found (coreutils, diffutils); the tools suite needs it"
    done ;;
  le|all)
    command -v timeout >/dev/null 2>&1 || die "timeout not found (coreutils); the low-entropy runs need it"
    command -v patch >/dev/null 2>&1 || die "patch not found; the low-entropy runs need it"
    command -v cmp >/dev/null 2>&1 || die "cmp not found (diffutils); the low-entropy runs need it" ;;
  *) usage ;;
esac

disksize=$((50 * GIB))
[ "$testing" != 1 ] || disksize=${RIFT_BENCH_DISK_SIZE:-$disksize}
diskmargin=$((10 * GIB))
diskneed=$((2 * disksize + diskmargin))
# The tools suite puts only its 4 GiB pairs on the disk.
[ "$suite" != tools ] || diskneed=$((8 * GIB + diskmargin))

is_tmpfs() { [ "$testing" = 1 ] || [ "$(stat -f -c %T "$1" 2>/dev/null)" = tmpfs ]; }
free_bytes() { df -Pk "$1" | awk 'NR==2 {print $4 * 1024}'; }

# ---- --disk-dir checks: a real disk with room for both files ---------------

if [ -n "$diskdir" ]; then
  [ -d "$diskdir" ] || die "--disk-dir $diskdir is not a directory"
  [ -w "$diskdir" ] || die "--disk-dir $diskdir is not writable"
  disk_fstype=$(stat -f -c %T "$diskdir" 2>/dev/null || echo unknown)
  [ "$testing" != 1 ] || [ "$(uname -s)" = Linux ] || disk_fstype=unknown
  case $disk_fstype in
    tmpfs|ramfs) die "--disk-dir $diskdir is on $disk_fstype (RAM); the point is a real disk" ;;
    unknown) [ "$testing" = 1 ] || die "cannot read the file system type of $diskdir" ;;
  esac
  disk_free=$(df -Pk "$diskdir" | awk 'NR==2 {print $4 * 1024}')
  [ "$disk_free" -ge "$diskneed" ] ||
    die "--disk-dir $diskdir has $((disk_free / 1048576)) MiB free, needs $((diskneed / 1048576)) MiB (two files of $((disksize / 1048576)) MiB plus a $((diskmargin / 1048576)) MiB margin)"
  diskroot=$(cd "$diskdir" && pwd)
  # The tools suite puts this path into DuckDB's SQL in single quotes.
  case $diskroot in *"'"*) die "--disk-dir $diskroot has a single quote in its path" ;; esac
  # Block device and model, when /sys shows them without root. LVM, btrfs
  # multi-device and network mounts have no single model; the report says so.
  disk_dev=$(df -P "$diskroot" | awk 'NR==2 {print $1}')
  disk_model=unreadable disk_rot=unreadable
  sysp=$(readlink -f "/sys/class/block/${disk_dev##*/}" 2>/dev/null || true)
  if [ -n "$sysp" ]; then
    [ ! -e "$sysp/partition" ] || sysp=$(dirname "$sysp")
    disk_model=$(tr -s ' ' < "$sysp/device/model" 2>/dev/null | sed 's/ $//' || true)
    disk_rot=$(cat "$sysp/queue/rotational" 2>/dev/null || true)
    case $disk_rot in 0) disk_rot=ssd ;; 1) disk_rot=rotational ;; *) disk_rot=unreadable ;; esac
    [ -n "$disk_model" ] || disk_model=unreadable
  fi
fi

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
[ "$maxg" -gt 0 ] || [ "$suite" = le ] || die "refusing to start: a 1 GiB pair needs $(( (2 * GIB + GIB + 2 * GIB) * 100 / 60 / 1048576 )) MiB of MemAvailable (60% rule), this host has $((avail / 1048576)) MiB"

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

# Low-entropy pairs of 64 MiB, 256 MiB and 1 GiB, each kept if the pair and
# the patch cap (the pair's size plus 1 GiB, more than rift's 1 GiB) fit the
# 60% rule. The diff file is sized only after the measured run, so whether
# the round trip fits is checked then, with the diff's real size.
le_sizes=
for s in $((GIB / 16)) $((GIB / 4)) "$GIB"; do
  if fits "$s" $((s + GIB)); then le_sizes="$le_sizes $s"; fi
done
[ "$testing" != 1 ] || le_sizes=${RIFT_BENCH_LE_SIZES:-$le_sizes}
le_sizes=${le_sizes# }
case $suite in standard|tools) true ;; *) false ;; esac || [ -n "$le_sizes" ] ||
  die "refusing to start: not even a 64 MiB low-entropy pair fits within 60% of MemAvailable"
le_points="distinct-1 distinct-2 distinct-10 distinct-100 distinct-10000 distinct-1000000 periodic-1000 runs alternating empty"
le_small=${le_sizes%% *} le_diffcap=$((2 * GIB)) le_tmo=600
if [ "$limiter" != none ] && fits "$le_small" "$le_diffcap"; then le_diffok=1; else le_diffok=0; fi
# Loghub BGL, the message part of each line: see bench/data.sh.
bgl_url='https://zenodo.org/records/8196385/files/BGL.zip?download=1'
bgl_zip_sha256=d67fd82a711aea0157a9b83175892c6ee60e384a2ddf5bc51f39118453816da8
bgl_sha256=a56a414f612d729db654ce36f7dfc1672fe1686a1727f5ccd9335f21bdfe0323
bgl_size=313217616
if command -v curl >/dev/null 2>&1 && command -v unzip >/dev/null 2>&1 &&
   command -v sha256sum >/dev/null 2>&1; then bgl_tools=1; else bgl_tools=0; fi
if [ "$bgl_tools" = 1 ] && fits "$bgl_size" $((bgl_size + GIB)); then bglok=1; else bglok=0; fi

# Tools suite: each pair size in tmpfs when the pair, a cap and the 2 GiB
# margin fit the 60% rule with a cap of at least 2 GiB; else on --disk-dir.
# The cap is 12 GiB or the most the rule leaves.
tool_tmo=600
[ "$testing" != 1 ] || tool_tmo=${RIFT_BENCH_TOOLS_TIMEOUT:-$tool_tmo}
tool_sizes="$((GIB / 4)) $GIB $((4 * GIB))"
[ "$testing" != 1 ] || tool_sizes=${RIFT_BENCH_TOOLS_SIZES:-$tool_sizes}
tool_tl=$GIB
[ "$testing" != 1 ] || tool_tl=${RIFT_BENCH_TOOLS_TIMELINE:-$tool_tl}
min_cap() { if [ "$1" -lt $((12 * GIB)) ]; then echo "$1"; else echo $((12 * GIB)); fi; }
tool_plan=
for s in $tool_sizes; do
  c=$(min_cap $((limit - 2 * s - 2 * GIB)))
  if [ "$c" -ge $((2 * GIB)) ] && [ $((2 * s + GIB)) -le "$tmpfs_free" ]; then
    tool_plan="$tool_plan $s:tmpfs:$c"
  elif [ -n "$diskroot" ] && [ $((limit - 2 * GIB)) -ge $((2 * GIB)) ]; then
    # On disk the pair is page cache, which the kernel can drop.
    tool_plan="$tool_plan $s:disk:$(min_cap $((limit - 2 * GIB)))"
  else
    tool_plan="$tool_plan $s:none:0"
  fi
done
tool_plan=${tool_plan# }
# Optional tools: each row runs only if the tool is on PATH.
have() { command -v "$1" >/dev/null 2>&1; }
tool_version() {
  case $1 in
    diff|cmp|sort) "$1" --version 2>/dev/null | head -n1 ;;
    git) git --version ;;
    difft) difft --version 2>/dev/null | head -n1 ;;
    duckdb) duckdb --version 2>/dev/null | head -n1 ;;
    csvdiff) csvdiff --version 2>/dev/null | head -n1 ;;
    qsv) qsv --version 2>/dev/null | cut -d' ' -f1-2 ;;
    daff) echo "daff $(daff version 2>/dev/null | head -n1)" ;;
  esac
}
[ "$suite" != tools ] || [ "$testing" = 1 ] || [ "$limiter" = systemd ] ||
  die "the tools suite needs a systemd user scope for its caps; prlimit's address-space cap would stop DuckDB and qsv, which reserve far more address space than they use"

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
  echo "  suite             $suite"
  echo "  repetitions       $reps of each repeated run"
  case $limiter in
    systemd) if [ "$suite" = tools ]; then echo "  memory cap        systemd-run --user scope, MemoryMax, MemorySwapMax=0: per pair size, below"
             else echo "  memory cap        systemd-run --user scope, MemoryMax, MemorySwapMax=0: rift 1 GiB (1.5 GiB for --mem 1G)"; fi ;;
    prlimit) echo "  memory cap        prlimit --as (systemd user scope unavailable): address space, not RSS" ;;
    none) echo "  memory cap        none available (no systemd user scope, no prlimit): diff and git diff skipped" ;;
  esac
  case $suite in standard|all) plan_standard ;; tools) plan_tools ;; esac
  case $suite in le|all) plan_le ;; esac
  [ -z "$ropair" ] || echo "  read-only pair    $ropair (only read)"
  if [ -n "$diskdir" ] && [ "$suite" != tools ]; then
    echo "  on-disk pair      $(gib "$disksize") each of A and B (profile sql, seed 7, 0.1% changes) in $diskroot ($disk_fstype on $disk_dev, $disk_model, $disk_rot)"
    echo "  disk use          $(gib $((2 * disksize))) written, $(gib "$disk_free") free, $(gib "$diskneed") required (pair plus $(gib "$diskmargin") margin); deleted afterwards"
    echo "  disk runs         cat a b | wc -c, rift --summary, rift --stats, all --mem 256M, one run each, cap 1 GiB; diff and git diff skipped: they need several times the pair in memory"
    echo "  disk time         about $(awk -v b="$((2 * disksize))" 'BEGIN { printf "%d to %d minutes", b * 4 / 500e6 / 60, b * 4 / 100e6 / 60 }') (four passes over the pair at 500 to 100 MB/s, plus the build and the other runs)"
  fi
  if [ "$cold" = 1 ]; then
    ngov=$(cat "$sysfs_cpu"/cpu[0-9]*/cpufreq/scaling_governor 2>/dev/null | wc -l | tr -d ' ')
    cold_has_disk_group || echo "  warning           --cold: no on-disk group (--read-only-pair, or --disk-dir without --suite tools): no page cache will be dropped, only the governor is set"
    echo "  cold runs         page cache dropped before each timed run of the on-disk groups, not before tmpfs runs (sync, then sudo -n, echo 3 > drop_caches); needs sudo -v first"
    if [ "$ngov" -gt 0 ]; then
      echo "  governor          performance on $ngov CPUs for the run (now: $(cat "$sysfs_cpu"/cpu[0-9]*/cpufreq/scaling_governor | sort -u | tr '\n' ' ' | sed 's/ $//')), each CPU's own restored afterwards"
    else
      echo "  governor          no cpufreq files under $sysfs_cpu: only the cache drops"
    fi
  fi
  echo "  output            rift-bench-$label.tar.gz in the current directory"
}

plan_standard() {
  echo "  pair sizes        $(for s in $sizes; do printf '%s ' "$(gib "$s")"; done)(A and B each), profiles sql and csv, seed 7, 0.1% changes"
  echo "  tmpfs peak        $(gib $((2 * ${sizes##* })))"
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
}

plan_tools() {
  echo "  pairs             seed 7, 0.1% changes; profiles sql and csv for the line tools, shuffled csv for the key tools"
  for e in $tool_plan; do
    s=${e%%:*} w=${e#*:}; c=${w#*:}; w=${w%%:*}
    case $w in
      tmpfs) echo "    $(gib "$s")           in tmpfs, cap $(gib "$c")" ;;
      disk) echo "    $(gib "$s")           on $diskroot (page cache not dropped), cap $(gib "$c")" ;;
      none) echo "    $(gib "$s")           not run: the pair and a 2 GiB cap do not fit within 60% of MemAvailable; --disk-dir puts it on a disk" ;;
    esac
  done
  echo "  line tools        rift --stats --mem 256M, diff, git diff --no-index, git diff --no-index --text, difft, cat a b | wc -c, cmp, sort | comm -3"
  echo "  key tools         rift --key csv:id, DuckDB (FULL OUTER JOIN and EXCEPT, each at its default memory_limit and at 256MB), csvdiff, qsv diff, daff (256 MiB only)"
  echo "  runs              $reps of each; any run is stopped after $tool_tmo s; a command that is killed, stopped or prints no counts skips its later runs and larger sizes"
  echo "  RSS timeline      every 100 ms on the 1 GiB pairs: rift, diff and difft on sql, rift --key and DuckDB on the key pair"
  for t in diff git difft duckdb csvdiff qsv daff; do
    if have "$t"; then echo "  $(printf '%-17s' "$t")$(tool_version "$t")"; else echo "  $(printf '%-17s' "$t")not installed: its rows are skipped"; fi
  done
}

plan_le() {
  echo "  low-entropy       pairs of $(for s in $le_sizes; do printf '%s ' "$(gib "$s")"; done)(A and B each), seed 7, 0.1% changes:"
  echo "                    $le_points"
  echo "                    per pair: rift --stats --mem 256M (cap 1 GiB), then rift again into a patch"
  echo "                    file and patch -o - | cmp against B (cap: pair size + 1 GiB) if the diff fits;"
  echo "                    any run is stopped after $le_tmo s, and a point that fails skips its larger sizes"
  if [ "$le_diffok" = 1 ]; then
    echo "  GNU diff          changed lines on the $(gib "$le_small") pairs, and on BGL if it fits, cap $(gib "$le_diffcap")"
  else
    echo "  GNU diff          skipped: no limiter, or a $(gib "$le_diffcap") cap does not fit the 60% budget"
  fi
  if [ "$bglok" = 1 ]; then
    echo "  Loghub BGL        $(gib "$bgl_size") pair: the zip from Zenodo record 8196385 (CC BY 4.0), checked by sha256, B by spike mutate"
  elif [ "$bgl_tools" = 0 ]; then
    echo "  Loghub BGL        skipped: needs curl, unzip and sha256sum"
  else
    echo "  Loghub BGL        skipped: a $(gib "$bgl_size") pair does not fit the 60% budget"
  fi
}

plan
[ "$dry" = 0 ] || exit 0

if [ "$yes" != 1 ]; then
  echo "Starting in 10 s. Ctrl-C to cancel." >&2
  sleep 10
fi

# ---- scratch directory and cleanup -----------------------------------------

scratch=$base/rift-bench-$$
diskwork=
cleanup() {
  cold_restore
  cold_keep_stop
  case $scratch in
    */rift-bench-*) rm -rf "$scratch" ;;
  esac
  case $diskwork in
    */rift-bench-disk-*) rm -rf "$diskwork" ;;
  esac
}
# Arm the cleanup only once the directory is ours: if it already exists,
# mkdir fails and the existing directory is left alone.
mkdir -m 700 "$scratch" || die "$scratch already exists; remove it or rerun"
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
trap 'exit 129' HUP
[ "$cold" = 0 ] || cold_keepalive

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
if { [ "$suite" = le ] || [ "$suite" = all ]; } && [ "$bglok" = 1 ]; then
  # shellcheck disable=SC2086
  (cd "$scratch/src" && $niceio cargo build --release $locked -p rift-core --example spike) >>"$raw/build.log" 2>&1 ||
    { tail -n 20 "$raw/build.log" >&2; die "build of the spike example failed"; }
  cp "$scratch/target/release/examples/spike" "$scratch/bin/"
fi
rm -rf "$scratch/target" "$scratch/cargo" "$scratch/src"
rift=$scratch/bin/rift gen=$scratch/bin/rift-gen spike=$scratch/bin/spike
if [ "$suite" = le ] || [ "$suite" = all ]; then
  "$gen" --help | grep -q -- --distinct || die "rift-gen at $ref has no low-entropy profiles; pass a --ref that has them"
fi

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
fact mem_total_bytes "$mem_total"
fact mem_available_bytes "$avail"
fact kernel "$(uname -r)"
# shellcheck disable=SC1091
fact distro "$(. /etc/os-release 2>/dev/null && echo "${PRETTY_NAME:-unknown}" || echo unknown)"
fact virt "$(systemd-detect-virt 2>/dev/null || echo unknown)"
fact rustc "$rustc_v"
fact suite "$suite"
fact reps "$reps"
fact rift_ref "$ref"
fact rift_commit "$commit"
fact scratch_fs "tmpfs, $(gib "$tmpfs_free") free of the mount at start"
fact limiter "$limiter"
if [ -n "$diskdir" ]; then
  fact disk_pair_fs "$disk_fstype"
  fact disk_pair_device "$disk_dev"
  fact disk_pair_model "$disk_model ($disk_rot)"
  fact disk_pair_size_bytes "$disksize"
fi
fact load_before "$(loadavg)"
if [ "$cold" = 1 ]; then
  cold_set
  if [ -n "$cold_saved" ]; then
    fact cold_governor "performance on every CPU for the runs; before: $(cold_vals scaling_governor); energy_performance_preference before: $(cold_vals energy_performance_preference)"
  else
    fact cold_governor "unchanged: no cpufreq files under $sysfs_cpu (VM or no cpufreq); cache drops only"
    note "--cold: no cpufreq files under $sysfs_cpu, so the governor was not changed; only the cache was dropped before each run"
  fi
  [ -z "$cold_skipped" ] || note "--cold: governor not readable, left alone: $cold_skipped"
fi
# After cold_set, so a cold report names the governor the runs used.
fact cpu_governor "$(cold_host_governor)"

# ---- measured runs ---------------------------------------------------------

n=0

# bytes of "peak heap 9.8 MiB of 256 MiB (10276044 bytes)" in FILE: the
# exact count where rift prints one, the rounded one before that.
heap_bytes() {
  awk '/^peak heap/ && match($0, /\([0-9]+ bytes\)/) { print substr($0, RSTART + 1, RLENGTH - 8); exit }
  /^peak heap/ {
    m = 1; if ($4 == "KiB") m = 1024; else if ($4 == "MiB") m = 1048576
    else if ($4 == "GiB") m = 1073741824; else if ($4 == "TiB") m = 1099511627776
    printf "%.0f", $3 * m; exit }' "$1" 2>/dev/null || true
}

# run_one GROUP PROFILE SIZE TOOL THREADS CAP REP MODE CMD...
# MODE: count (stdout counted by wc -c), file (stdout kept, for rift
# --summary), lines (stdout bytes and diff's changed lines, the ones
# starting with < or >), cmp (stdout compared with the file $x_cmp; for
# patch). time -v writes to its own file, so the command's stderr stays
# clean for rift's --stats line. $x_lines (lines of A plus B) and
# $x_changed are copied into the row as they are.
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
  # Once the cap kills a command on a pair, or the timeout stops it, its
  # later repetitions would only fail again: record them as skipped instead.
  key="$grp|$prof|$sz|$tool|$thr"
  if grep -qxF "$key" "$killed"; then
    echo "[$id] $grp $prof $(gib "$sz") $tool threads=$thr rep=$rep: skipped, killed or stopped before" >&2
    printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t\t\t\t\t\t\t\tskipped\t\t\t\t\t%s\t\n' \
      "$id" "$grp" "$prof" "$sz" "$tool" "$thr" "$cap" "$rep" "$*" >> "$runs"
    ec=
    return
  fi
  echo "[$id] $grp $prof $(gib "$sz") $tool threads=$thr rep=$rep" >&2
  cold_run_drop "$grp"
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
    lines) $lim "$timebin" -v -o "$b.time" "$@" 2> "$b.err" |
      LC_ALL=C awk '{ n += length($0) + 1 } /^[<>]/ { c++ } END { print n + 0, c + 0 }' > "$b.bytes" || true ;;
    counted) $lim "$timebin" -v -o "$b.time" "$@" 2> "$b.err" | "$x_counter" > "$b.bytes" || true ;;
    cmp)
      if $lim "$timebin" -v -o "$b.time" "$@" 2> "$b.err" | cmp -s - "$x_cmp"; then
        echo same > "$b.cmp"
      else
        echo differs > "$b.cmp"
      fi ;;
  esac
  ec=$(awk -F': ' '/Exit status/ {print $2}' "$b.time" 2>/dev/null || true)
  wall=$(awk -F': ' '/Elapsed/ {n = split($NF, t, ":"); s = 0; for (i = 1; i <= n; i++) s = s * 60 + t[i]; print s}' "$b.time" 2>/dev/null || true)
  user=$(awk -F': ' '/User time/ {print $2}' "$b.time" 2>/dev/null || true)
  sys=$(awk -F': ' '/System time/ {print $2}' "$b.time" 2>/dev/null || true)
  rss=$(awk -F': ' '/Maximum resident/ {print $2}' "$b.time" 2>/dev/null || true)
  result=$(scope_result "$unit")
  # timeout exits 124 when it stopped the command, and 137 when it had to
  # kill it; a command the cap killed also ends in 137.
  if [ "$1" = timeout ]; then
    case $ec in 124) result=timeout ;; 137) [ "$result" = oom-kill ] || result=timeout ;; esac
  fi
  case $result in oom-kill|timeout) echo "$key" >> "$killed" ;; esac
  heap=
  case $tool in
    *--summary*) heap=$(heap_bytes "$b.out") ;;
    *--stats*) heap=$(heap_bytes "$b.err") ;;
  esac
  approx='' changed=$x_changed round='' counts=''
  case $tool in
    *--stats*) approx=$(awk '/^approx / {print $4; exit}' "$b.err" 2>/dev/null || true) ;;
  esac
  case $mode in
    count) out=$(cat "$b.bytes") ;;
    file) out=$(wc -c < "$b.out" | tr -d ' ') ;;
    lines) read -r out changed < "$b.bytes" || true ;;
    counted) IFS=$(printf '\t') read -r out changed counts < "$b.bytes" || true
      [ "$changed" != - ] || changed=
      [ "$counts" != - ] || counts= ;;
    cmp)
      out=
      # patch exits 0 only when every hunk applied. A patch its cap killed
      # says nothing about rift's diff.
      if [ "$result" = oom-kill ]; then round=killed
      elif [ "$ec" = 0 ] && [ "$(cat "$b.cmp")" = same ]; then round=ok; else round=failed; fi ;;
  esac
  # diff, cmp, git and rift exit 1 when the files differ; above that the
  # tool failed (git diff --text exits 128 on files over 1 GiB), and the
  # counter's "-0 +0" for its empty output is not a count.
  if [ "$grp" = tools ] && [ "${ec:-0}" -gt 1 ]; then
    case $result in oom-kill|timeout) ;; *) counts="no result" ;; esac
  fi
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$id" "$grp" "$prof" "$sz" "$tool" "$thr" "$cap" "$rep" "$wall" "$user" "$sys" "$rss" "$ec" "$heap" "$out" "$result" \
    "$approx" "$x_lines" "$changed" "$round" "$(printf '%s' "$*" | tr '\t\n' '  ')" "$counts" >> "$runs"
}
x_lines='' x_changed='' x_cmp='' x_counter=''

# core_runs GROUP PROFILE SIZE A B: the runs every pair gets.
core_runs() {
  g=$1 p=$2 s=$3 a=$4 bb=$5
  for r in $(seq "$reps"); do
    # Into a pipe, not /dev/null: cat can hand a file to /dev/null without
    # copying it, which made the baseline read 2 GiB in 0.05 s.
    run_one "$g" "$p" "$s" "cat a b | wc -c" all "$GIB" "$r" count cat "$a" "$bb"
  done
  for r in $(seq "$reps"); do
    run_one "$g" "$p" "$s" "rift --summary --mem 256M" all "$GIB" "$r" file "$rift" --summary --mem 256M "$a" "$bb"
  done
  for r in $(seq "$reps"); do
    run_one "$g" "$p" "$s" "rift --stats --mem 256M" all "$GIB" "$r" count "$rift" --stats --mem 256M "$a" "$bb"
  done
  run_one "$g" "$p" "$s" "rift --stats --mem 64M" all "$GIB" 1 count "$rift" --stats --mem 64M "$a" "$bb"
  # 1 GiB of heap does not fit a 1 GiB cap; the extra 0.5 GiB comes out of the 2 GiB margin.
  run_one "$g" "$p" "$s" "rift --stats --mem 1G" all $((GIB + GIB / 2)) 1 count "$rift" --stats --mem 1G "$a" "$bb"
}

# The log lives in the scratch directory, which the exit trap removes.
gen_failed() { rc=$1; tail -n 20 "$2" >&2; die "rift-gen failed (exit $rc)"; }

small=${sizes%% *}
pair=$scratch/pair

# prep CAP CMD...: an unmeasured step that writes into tmpfs. tmpfs pages
# count against the cgroup of the process that writes them, so CAP covers
# what the step writes plus its own memory.
prep() {
  pc=$1; shift
  # shellcheck disable=SC2086
  if [ "$limiter" = systemd ]; then
    systemd-run --user --scope -q -p MemoryMax="$pc" -p MemorySwapMax=0 $niceio "$@"
  else
    $niceio "$@"
  fi
}

# gen_pair SIZE LABEL ARGS...: generate the pair into $pair; ARGS pick the
# profile.
gen_pair() {
  gs=$1 gl=$2; shift 2
  echo "Generating $gl $(gib "$gs")" >&2
  rm -rf "$pair"
  glog=$raw/gen-$gl-$gs.log
  # The cap covers the pair rift-gen writes plus its own 1 GiB.
  prep $((2 * gs + GIB)) "$gen" --seed 7 --size "$gs" "$@" --change-ratio 0.001 --out "$pair" > "$glog" 2>&1 ||
    gen_failed $? "$glog"
  cp "$pair/manifest.tsv" "$raw/manifest-$gl-$gs.tsv" 2>/dev/null || true
}

# ---- standard runs ---------------------------------------------------------

if [ "$suite" = standard ] || [ "$suite" = all ]; then
  # A pair small enough for diff and git diff to fit their cap, so all three
  # tools get real times on the same data.
  if [ "$cmpok" = 1 ]; then
    for prof in sql csv; do
      gen_pair "$cmp" "$prof" --profile "$prof"
      for r in $(seq "$reps"); do
        run_one compare "$prof" "$cmp" "rift --stats --mem 256M" all "$GIB" "$r" count "$rift" --stats --mem 256M "$pair/a" "$pair/b"
        run_one compare "$prof" "$cmp" "diff a b" all "$cmpcap" "$r" count diff "$pair/a" "$pair/b"
        run_one compare "$prof" "$cmp" "git diff --no-index a b" all "$cmpcap" "$r" count git diff --no-index "$pair/a" "$pair/b"
      done
      rm -rf "$pair"
    done
  else
    note "comparison on the $(gib "$cmp") pair not run: no limiter, or a $(gib "$cmpcap") cap does not fit within 60% of MemAvailable"
  fi
  for size in $sizes; do
    for prof in sql csv; do
      gen_pair "$size" "$prof" --profile "$prof"
      core_runs generated "$prof" "$size" "$pair/a" "$pair/b"

      if [ "$size" = "$small" ]; then
        for r in $(seq "$reps"); do
          if [ "$diffok" = 1 ]; then
            run_one generated "$prof" "$size" "diff a b" all "$diffcap" "$r" count diff "$pair/a" "$pair/b"
            run_one generated "$prof" "$size" "git diff --no-index a b" all "$diffcap" "$r" count git diff --no-index "$pair/a" "$pair/b"
          fi
        done
        [ "$diffok" = 1 ] || note "diff and git diff not run on $prof $(gib "$size"): no limiter, or a $(gib "$diffcap") cap does not fit within 60% of MemAvailable"
        # rift sizes its threads from available_parallelism, which follows the
        # CPU affinity mask, so taskset is what changes the thread count.
        if [ "$have_taskset" = 1 ]; then
          for k in 1 2 4 "$cpus"; do
            [ "$k" -le "$cpus" ] || continue
            if [ "$k" -eq "$cpus" ]; then pre=; thr=all; else pre="taskset -c 0-$((k - 1))"; thr=$k; fi
            for r in $(seq "$reps"); do
              # shellcheck disable=SC2086
              run_one scaling "$prof" "$size" "rift --summary --mem 256M" "$thr" "$GIB" "$r" file $pre "$rift" --summary --mem 256M "$pair/a" "$pair/b"
            done
          done
        else
          note "thread scaling not run: taskset not found"
        fi
      fi
      rm -rf "$pair"
    done
  done
fi

# ---- low-entropy runs ------------------------------------------------------

# le_run LABEL SIZE A B: the measured rift run, the round trip through patch,
# and on the smallest pairs GNU diff's changed lines. Returns 1 when rift
# failed, so the caller can skip the point's larger sizes.
le_run() {
  ll=$1 ls=$2 la=$3 lb=$4
  x_lines=$(($(wc -l < "$la") + $(wc -l < "$lb")))
  run_one le "$ll" "$ls" "rift --stats --mem 256M" all "$GIB" 1 count \
    timeout -k 10 "$le_tmo" "$rift" --stats --mem 256M "$la" "$lb"
  lrc=0 le_result=$result le_ec=$ec
  case $ec in
    0|1)
      d=$(cat "$raw/$id.bytes")
      # The patch file, the pair and the patch cap must fit the 60% rule
      # and the tmpfs, as everything else here.
      if fits $((ls + d / 2)) $((ls + GIB)); then
        dp=$pair/d.patch
        if prep $((GIB + d + 64 * 1048576)) timeout -k 10 "$le_tmo" "$rift" --mem 256M "$la" "$lb" > "$dp" 2> "$raw/$id.patch-run.err" ||
           [ $? -eq 1 ]; then
          # Header lines start with --- and +++; every other line that
          # starts with - or + is a changed line.
          x_changed=$(LC_ALL=C awk 'NR > 2 && /^[-+]/ { c++ } END { print c + 0 }' "$dp")
          [ "$(wc -c < "$dp" | tr -d ' ')" = "$d" ] ||
            note "$ll $(gib "$ls"): the second rift run wrote $(wc -c < "$dp" | tr -d ' ') bytes, the measured one $d"
          x_cmp=$lb
          run_one le "$ll" "$ls" "patch -o - a d.patch | cmp - b" all $((ls + GIB)) 1 cmp \
            timeout -k 10 "$le_tmo" patch -s -r - -o - "$la" "$dp"
          x_cmp='' x_changed=''
        else
          note "$ll $(gib "$ls"): the rift run that writes the patch file failed; no round trip"
        fi
        rm -f "$dp"
      else
        note "$ll $(gib "$ls"): a $(gib "$d") diff does not fit within 60% of MemAvailable; no round trip"
      fi ;;
    *) lrc=1 ;;
  esac
  if [ "$le_diffok" = 1 ] && fits "$ls" "$le_diffcap" && { [ "$ls" = "$le_small" ] || [ "$ll" = bgl ]; }; then
    # -a: BGL has a few NUL bytes, and diff would only say the files differ.
    run_one le "$ll" "$ls" "diff -a a b" all "$le_diffcap" 1 lines timeout -k 10 "$le_tmo" diff -a "$la" "$lb"
  fi
  x_lines=
  return "$lrc"
}

if [ "$suite" = le ] || [ "$suite" = all ]; then
  for pt in $le_points; do
    case $pt in
      distinct-*) set -- --profile distinct --distinct "${pt#distinct-}" ;;
      periodic-*) set -- --profile periodic --period "${pt#periodic-}" ;;
      *) set -- --profile "$pt" ;;
    esac
    for size in $le_sizes; do
      gen_pair "$size" "$pt" "$@"
      if ! le_run "$pt" "$size" "$pair/a" "$pair/b"; then
        note "$pt: rift failed or ran out of time on $(gib "$size") ($le_result, exit $le_ec); larger sizes not run"
        rm -rf "$pair"
        break
      fi
      rm -rf "$pair"
    done
  done
  if [ "$bglok" = 1 ]; then
    echo "Fetching Loghub BGL" >&2
    rm -rf "$pair"
    mkdir "$pair"
    zip=$scratch/bgl.zip
    curl -fsSL -o "$zip" "$bgl_url" || die "download of $bgl_url failed"
    [ "$(sha256sum "$zip" | cut -d' ' -f1)" = "$bgl_zip_sha256" ] || die "BGL.zip: sha256 mismatch"
    # unzip and cut write the 299 MiB file, spike mutate the same again.
    # shellcheck disable=SC2016
    prep $((bgl_size + GIB)) sh -c 'unzip -p "$1" BGL.log | LC_ALL=C cut -d" " -f7- > "$2"' sh "$zip" "$pair/a" ||
      die "unpacking BGL.zip failed"
    rm -f "$zip"
    [ "$(sha256sum "$pair/a" | cut -d' ' -f1)" = "$bgl_sha256" ] || die "bgl.log: sha256 mismatch"
    prep $((bgl_size + GIB)) "$spike" mutate "$pair/a" "$pair/b" 1 0.001 > "$raw/bgl-mutate.log" 2>&1 ||
      die "spike mutate failed on bgl.log"
    le_run bgl "$bgl_size" "$pair/a" "$pair/b" || note "bgl: rift failed or ran out of time ($le_result, exit $le_ec)"
    rm -rf "$pair"
  elif [ "$bgl_tools" = 0 ]; then
    note "Loghub BGL pair not run: needs curl, unzip and sha256sum"
  else
    note "Loghub BGL pair not run: it does not fit within 60% of MemAvailable"
  fi
fi

# ---- tools suite -----------------------------------------------------------

# Counters for run_one's counted mode: each reads a tool's output on stdin
# and prints bytes, changed lines or keys, and the counts by kind, tab-separated.
# A field with no value is "-": read would merge two tabs into one. A tool
# that printed no counts gets "no result", which the report lists as failed.
# Unified diffs: a hunk header says how many lines of A and B follow, so a
# deleted line that starts with "--" is not taken for a file header.
count_unified() {
  LC_ALL=C awk '{ n += length($0) + 1 }
  ra > 0 || rb > 0 { c = substr($0, 1, 1)
    if (c == "-") { d++; ra-- } else if (c == "+") { a++; rb-- } else if (c == " ") { ra--; rb-- }
    next }
  /^@@ / { split($2, x, ","); ra = (2 in x) ? x[2] + 0 : 1; split($3, y, ","); rb = (2 in y) ? y[2] + 0 : 1 }
  END { printf "%d\t%d\t-%d +%d\n", n, a + d, d, a }'
}
count_lines() { LC_ALL=C awk '{ n += length($0) + 1 } END { printf "%d\t%d\t-\n", n, NR }'; }
count_rift_keys() {
  LC_ALL=C awk '{ n += length($0) + 1; c = substr($0, 1, 1) } c == "+" { a++ } c == "-" { r++ } c == "~" { m++ }
  END { printf "%d\t%d\t+%d -%d ~%d\n", n, a + r + m, a, r, m }'
}
# DuckDB prints a header line and one row of counts after the memory limit.
count_duck_join() { LC_ALL=C awk -F, '{ n += length($0) + 1 } NR == 2 { a = $2; r = $3; m = $4; l = $1; ok = 1 }
  END { if (ok) printf "%d\t%d\t+%d -%d ~%d (memory_limit %s)\n", n, a + r + m, a, r, m, l; else printf "%d\t-\tno result\n", n }'; }
count_duck_except() { LC_ALL=C awk -F, '{ n += length($0) + 1 } NR == 2 { x = $2; y = $3; l = $1; ok = 1 }
  END { if (ok) printf "%d\t%d\trows only in A %d, only in B %d (memory_limit %s)\n", n, x + y, x, y, l; else printf "%d\t-\tno result\n", n }'; }
# csvdiff prints its counts on stderr, which the command line sends here.
count_csvdiff() {
  LC_ALL=C awk '{ n += length($0) + 1 } /^# (Additions|Modifications|Deletions) \(/ { v = $3; gsub(/[()]/, "", v); k[$2] = v; ok = 1 }
  END { if (ok) printf "%d\t%d\t+%d -%d ~%d\n", n, k["Additions"] + k["Deletions"] + k["Modifications"], k["Additions"], k["Deletions"], k["Modifications"]; else printf "%d\t-\tno result\n", n }'
}
# qsv diff writes a changed row as a - row and a + row.
count_qsv() { LC_ALL=C awk -F, '{ n += length($0) + 1 } NR == 1 { ok = ($NF == "diffresult" || $1 == "diffresult") } NR > 1 && $1 == "+" { a++ } NR > 1 && $1 == "-" { r++ }
  END { if (ok) printf "%d\t-\trows + %d, - %d\n", n, a, r; else printf "%d\t-\tno result\n", n }'; }
count_daff() { LC_ALL=C awk -F, '{ n += length($0) + 1 } $1 == "@@" { ok = 1 } $1 == "+++" { a++ } $1 == "---" { r++ } $1 == "->" { m++ }
  END { if (ok) printf "%d\t%d\t+%d -%d ~%d\n", n, a + r + m, a, r, m; else printf "%d\t-\tno result\n", n }'; }

tfail=$scratch/tools-failed.txt
: > "$tfail"

# tool_rows PROFILE SIZE TOOL CAP MODE COUNTER CMD...: the repetitions of one
# row, each stopped after $tool_tmo s. A command that was killed, stopped or
# printed no counts on a smaller pair is not run on this one.
tool_rows() {
  tp=$1 ts=$2 tt=$3 tc=$4 tm=$5; x_counter=$6; shift 6
  if grep -qxF "$tp|$tt" "$tfail"; then
    note "$tt on $tp $(gib "$ts") not run: it failed on a smaller pair"
    return
  fi
  for r in $(seq "$reps"); do
    run_one tools "$tp" "$ts" "$tt" all "$tc" "$r" "$tm" timeout -k 10 "$tool_tmo" "$@"
    case $result in oom-kill|timeout) echo "$tp|$tt" >> "$tfail" ;; esac
    [ "$counts" != "no result" ] || echo "$tp|$tt" >> "$tfail"
  done
  x_counter=
}

# timeline NAME CAP CMD...: VmRSS of the command every 100 ms, as
# milliseconds and KiB, until it ends or $tool_tmo s pass. systemd-run
# --scope, nice and ionice exec the command, so $! is its pid.
timeline() {
  tn=$1 tc=$2; shift 2
  out=$raw/timeline-$tn.tsv
  echo "timeline $tn" >&2
  # shellcheck disable=SC2086
  case $limiter in
    systemd) systemd-run --user --scope -q -p MemoryMax="$tc" -p MemorySwapMax=0 $niceio "$@" > /dev/null 2>&1 & ;;
    *) $niceio "$@" > /dev/null 2>&1 & ;;
  esac
  tp=$!
  t0=$(date +%s%N)
  printf 'ms\trss_kib\n' > "$out"
  while kill -0 "$tp" 2>/dev/null; do
    t=$(( ($(date +%s%N) - t0) / 1000000 ))
    awk -v t="$t" '/^VmRSS:/ { print t "\t" $2 }' "/proc/$tp/status" >> "$out" 2>/dev/null || true
    if [ "$t" -gt $((tool_tmo * 1000)) ]; then kill "$tp" 2>/dev/null || true; echo "stopped after $tool_tmo s" >> "$out"; fi
    sleep 0.1
  done
  wait "$tp" 2>/dev/null || true
}

# One line each: the query is part of the command recorded in runs.tsv.
duck_join() {
  tr '\n' ' ' <<SQL
SET temp_directory='$scratch/duck.tmp'; $1
SELECT current_setting('memory_limit') AS memory_limit,
  count(*) FILTER (WHERE a.id IS NULL) AS added,
  count(*) FILTER (WHERE b.id IS NULL) AS removed,
  count(*) FILTER (WHERE a.id IS NOT NULL AND b.id IS NOT NULL AND a IS DISTINCT FROM b) AS changed
FROM read_csv('$2', all_varchar = true) a FULL OUTER JOIN read_csv('$3', all_varchar = true) b ON a.id = b.id;
SQL
}
duck_except() {
  tr '\n' ' ' <<SQL
SET temp_directory='$scratch/duck.tmp'; $1
SELECT current_setting('memory_limit') AS memory_limit,
  (SELECT count(*) FROM (FROM read_csv('$2', all_varchar = true) EXCEPT FROM read_csv('$3', all_varchar = true))) AS only_a,
  (SELECT count(*) FROM (FROM read_csv('$3', all_varchar = true) EXCEPT FROM read_csv('$2', all_varchar = true))) AS only_b;
SQL
}

if [ "$suite" = tools ]; then
  for t in diff git difft duckdb csvdiff qsv daff sort cmp; do
    if have "$t"; then fact "version_$t" "$(tool_version "$t")"; else fact "version_$t" "not installed"; fi
  done
  fact tool_timeout_s "$tool_tmo"
  have difft || note "difftastic not installed: its rows are skipped"
  have duckdb || note "DuckDB not installed: its rows are skipped"
  have csvdiff || note "csvdiff not installed: its rows are skipped"
  have qsv || note "qsv not installed: its rows are skipped"
  have daff || note "daff not installed: its rows are skipped"
  first=${tool_plan%% *}; first=${first%%:*}
  for e in $tool_plan; do
    ts=${e%%:*} w=${e#*:}; tc=${w#*:}; w=${w%%:*}
    case $w in
      none) note "tools: $(gib "$ts") pairs not run: the pair and a 2 GiB cap do not fit within 60% of MemAvailable (--disk-dir puts them on a disk)"; continue ;;
      disk)
        mkdir -m 700 "$diskroot/rift-bench-disk-$$" 2>/dev/null || [ -d "$diskroot/rift-bench-disk-$$" ] ||
          die "cannot create a directory in $diskroot"
        diskwork=$diskroot/rift-bench-disk-$$
        pair=$diskwork/pair
        note "tools: the $(gib "$ts") pairs were on $diskroot ($disk_fstype, $disk_model), page cache not dropped; the other sizes were in tmpfs" ;;
      tmpfs) pair=$scratch/pair ;;
    esac
    for prof in sql csv; do
      gen_pair "$ts" "$prof" --profile "$prof"
      a=$pair/a bb=$pair/b
      tool_rows "$prof" "$ts" "cat a b | wc -c" "$tc" count '' cat "$a" "$bb"
      tool_rows "$prof" "$ts" "cmp a b" "$tc" count '' cmp "$a" "$bb"
      tool_rows "$prof" "$ts" "rift --stats --mem 256M" "$tc" counted count_unified "$rift" --stats --mem 256M "$a" "$bb"
      tool_rows "$prof" "$ts" "diff a b" "$tc" lines '' diff "$a" "$bb"
      tool_rows "$prof" "$ts" "git diff --no-index a b" "$tc" counted count_unified git diff --no-index "$a" "$bb"
      tool_rows "$prof" "$ts" "git diff --no-index --text a b" "$tc" counted count_unified git diff --no-index --text "$a" "$bb"
      have difft && tool_rows "$prof" "$ts" "difft a b" "$tc" count '' difft --color never "$a" "$bb"
      # Both sorted copies go next to the pair, so they count against the cap.
      # shellcheck disable=SC2016
      tool_rows "$prof" "$ts" "sort a, sort b, comm -3" "$tc" counted count_lines \
        sh -c 'LC_ALL=C sort -T "$3" "$1" > "$3/sa" && LC_ALL=C sort -T "$3" "$2" > "$3/sb" && LC_ALL=C comm -3 "$3/sa" "$3/sb"; s=$?; rm -f "$3/sa" "$3/sb"; exit $s' \
        sh "$a" "$bb" "$pair"
      if [ "$ts" = "$tool_tl" ] && [ "$prof" = sql ]; then
        timeline rift-sql "$tc" "$rift" --stats --mem 256M "$a" "$bb"
        timeline diff-sql "$tc" diff "$a" "$bb"
        have difft && timeline difft-sql "$tc" difft --color never "$a" "$bb"
      fi
      rm -rf "$pair"
    done
    gen_pair "$ts" keys --profile shuffled --rows csv
    # qsv reads only files whose name says CSV.
    mv "$pair/a" "$pair/a.csv"; mv "$pair/b" "$pair/b.csv"
    a=$pair/a.csv bb=$pair/b.csv
    mkdir -p "$scratch/duck.tmp"
    tool_rows keys "$ts" "rift --key csv:id" "$tc" counted count_rift_keys "$rift" --key csv:id "$a" "$bb"
    if have duckdb; then
      tool_rows keys "$ts" "duckdb full outer join" "$tc" counted count_duck_join duckdb -csv -c "$(duck_join '' "$a" "$bb")"
      tool_rows keys "$ts" "duckdb full outer join, memory_limit 256MB" "$tc" counted count_duck_join duckdb -csv -c "$(duck_join "SET memory_limit='256MB';" "$a" "$bb")"
      tool_rows keys "$ts" "duckdb except" "$tc" counted count_duck_except duckdb -csv -c "$(duck_except '' "$a" "$bb")"
      tool_rows keys "$ts" "duckdb except, memory_limit 256MB" "$tc" counted count_duck_except duckdb -csv -c "$(duck_except "SET memory_limit='256MB';" "$a" "$bb")"
    fi
    # shellcheck disable=SC2016
    have csvdiff && tool_rows keys "$ts" "csvdiff -p 0" "$tc" counted count_csvdiff sh -c 'exec csvdiff "$1" "$2" -p 0 2>&1' sh "$a" "$bb"
    have qsv && tool_rows keys "$ts" "qsv diff -k 0" "$tc" counted count_qsv qsv diff -k 0 "$a" "$bb"
    # daff took 7.9 s on a 4 MiB pair on an Apple M1; larger pairs would only time out.
    if have daff && [ "$ts" = "$first" ]; then
      tool_rows keys "$ts" "daff diff --id id" "$tc" counted count_daff daff diff --id id --output-format csv "$a" "$bb"
    fi
    if [ "$ts" = "$tool_tl" ]; then
      timeline rift-keys "$tc" "$rift" --key csv:id "$a" "$bb"
      have duckdb && timeline duckdb-keys "$tc" duckdb -csv -c "$(duck_join '' "$a" "$bb")"
    fi
    rm -rf "$pair" "$scratch/duck.tmp"
    if [ "$w" = disk ]; then rm -rf "$diskwork"; diskwork=; fi
  done
  note "tools: diff and difft run on one thread; rift, DuckDB and qsv use every CPU. difftastic is built for code: above its size limits it falls back to a line diff, which its output names \"Text\". git diff --no-index calls files over 512 MiB binary and prints no diff unless given --text. cmp stops at the first differing byte, so its time is not a read of both files. sort | comm compares sorted lines as sets, so moved lines do not count as changes. DuckDB spills to $scratch/duck.tmp, which is tmpfs, so a spill is RAM charged to its cap."
fi

if [ -n "$ropair" ]; then
  rsz=$(wc -c < "$ropair/a" | tr -d ' ')
  core_runs read-only-pair existing "$rsz" "$ropair/a" "$ropair/b"
fi

# ---- on-disk pair (--disk-dir) ---------------------------------------------

if [ -n "$diskdir" ] && [ "$suite" != tools ]; then
  # Set only after mkdir succeeds, so the trap never removes a directory it
  # did not create. Interrupt, error or exit removes the pair.
  mkdir -m 700 "$diskroot/rift-bench-disk-$$" || die "cannot create a directory in $diskroot"
  diskwork=$diskroot/rift-bench-disk-$$
  dpair=$diskwork/pair
  echo "Generating sql $(gib "$disksize") into $diskwork" >&2
  dlog=$raw/gen-disk-sql-$disksize.log
  # shellcheck disable=SC2086
  if [ "$limiter" = systemd ]; then
    # Written pages are reclaimable page cache; 2 GiB leaves rift-gen room.
    dgen="systemd-run --user --scope -q -p MemoryMax=$((2 * GIB)) -p MemorySwapMax=0 $niceio"
  else
    dgen=$niceio
  fi
  # shellcheck disable=SC2086
  if $dgen "$gen" --seed 7 --size "$disksize" --profile sql --change-ratio 0.001 --out "$dpair" > "$dlog" 2>&1; then
    cp "$dpair/manifest.tsv" "$raw/manifest-disk-sql-$disksize.tsv" 2>/dev/null || true
    # One run per tool: at this size a repetition costs minutes to hours.
    run_one disk sql "$disksize" "cat a b | wc -c" all "$GIB" 1 count cat "$dpair/a" "$dpair/b"
    run_one disk sql "$disksize" "rift --summary --mem 256M" all "$GIB" 1 file "$rift" --summary --mem 256M "$dpair/a" "$dpair/b"
    run_one disk sql "$disksize" "rift --stats --mem 256M" all "$GIB" 1 count "$rift" --stats --mem 256M "$dpair/a" "$dpair/b"
  else
    tail -n 20 "$dlog" >&2
    note "on-disk pair not run: rift-gen failed in $diskroot (log in raw/)"
  fi
  rm -rf "$diskwork"; diskwork=
  note "on-disk pair: $disk_fstype on $disk_dev, $disk_model ($disk_rot). $(if [ "$cold" = 1 ]; then echo "The page cache was dropped before each timed run of this group (--cold)."; else echo "The script cannot drop the page cache without root (no --cold), so the cache state is whatever the host had: the pair was written just before the first read, so the last pages of B may be cached."; fi) A and B together are $(gib $((2 * disksize))) against $(gib "$mem_total") of RAM$(cold_ram_clause $((2 * disksize)) "$mem_total") Reads run at idle I/O priority (ionice -c3), which can lengthen them on a busy disk. diff and git diff were not run: GNU diff holds far more than the 1 GiB rift cap for files this size."
fi

fact load_after "$(loadavg)"
[ "$limiter" != prlimit ] || note "no systemd user scope: caps are prlimit --as address-space limits, which are stricter than RSS caps"

# ---- report ----------------------------------------------------------------

if [ "$cold" = 1 ]; then
  fact cold_cache "$(cold_cache_desc)"
  g_before=$(cold_vals scaling_governor)
  cold_restore
  if [ -n "$cold_before" ]; then
    fact cold_governor_restored "$(cold_vals scaling_governor now) (was $g_before); energy_performance_preference now: $(cold_vals energy_performance_preference now)${cold_failed:+; NOT RESTORED: $cold_failed}"
    [ -z "$cold_failed" ] || note "--cold: restore failed, the host was left on: $cold_failed"
  fi
fi
pkg=$scratch/pkg
# Input lines are tagged F (fact), N (note) or R (run) so one awk can read all three.
{ awk '{print "F\t" $0}' "$facts"; awk '{print "N\t" $0}' "$notes"; awk '{print "R\t" $0}' "$runs"; } | awk -F'\t' -v schema="$schema" '
function esc(s) { gsub(/\\/, "\\\\", s); gsub(/"/, "\\\"", s); gsub(/[\001-\037]/, " ", s); return s }
function num(x) { return x == "" ? "null" : x }
$1 == "F" { f[++nf] = $2; v[nf] = $3; next }
$1 == "N" { nt[++nn] = $2; next }
{
  share = ($18 != "" && $19 > 0) ? sprintf("%.6f", $18 / $19) : ""
  r[++nr] = sprintf("    {\"id\": \"%s\", \"group\": \"%s\", \"profile\": \"%s\", \"size_bytes\": %s, \"tool\": \"%s\", \"threads\": \"%s\", \"cap_bytes\": %s, \"rep\": %s, \"wall_s\": %s, \"user_s\": %s, \"sys_s\": %s, \"max_rss_kb\": %s, \"exit_code\": %s, \"rift_peak_heap_bytes\": %s, \"output_bytes\": %s, \"result\": \"%s\", \"oom_killed\": %s, \"approx_lines\": %s, \"lines_ab\": %s, \"approx_share\": %s, \"changed_lines\": %s, \"roundtrip\": %s, \"command\": \"%s\", \"counts\": \"%s\"}",
    $2, $3, $4, num($5), esc($6), $7, num($8), num($9), num($10), num($11), num($12), num($13), num($14), num($15), num($16), $17, ($17 == "oom-kill" ? "true" : "false"),
    num($18), num($19), num(share), num($20), ($21 == "" ? "null" : "\"" $21 "\""), esc($22), esc($23))
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
  [ -z "$diskdir" ] || [ "$suite" = tools ] || echo "Exception: the group \`disk\` ran on a pair stored on disk ($disk_fstype); see the notes."
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
  if awk -F'\t' '$2 == "tools" { f = 1 } END { exit !f }' "$runs"; then
    echo
    echo "## Tool comparison"
    echo
    echo "One row per command and pair. Time is the median wall time of the runs that finished, with the fastest and slowest; RSS is the median peak resident set; counts are what the tool reported, as changed lines (- and +) or keys (+ added, - removed, ~ changed); failed lists the runs the cap killed (oom-kill), the timeout stopped (timeout), that printed no counts or exited above 1 (no result, with the exit code; the tool's message is in raw/ID.err), or that were skipped after a kill or a stop. Every tool's version is under Host."
    echo
    echo "| pair | size | tool | runs | time | fastest | slowest | RSS | peak heap | output | counts | failed |"
    echo "|---|---|---|---|---|---|---|---|---|---|---|---|"
    LC_ALL=C sort -t "$(printf '\t')" -k3,3 -k4,4n -k1,1 "$runs" | awk -F'\t' '
    function h(b,   u, i) { if (b == "") return ""; split("B KiB MiB GiB TiB", u, " "); i = 1; while (b >= 1024 && i < 5) { b /= 1024; i++ } return sprintf(i == 1 ? "%d %s" : "%.1f %s", b, u[i]) }
    function med(s,   v, n, i, j, t) { n = split(s, v, " "); for (i = 2; i <= n; i++) { t = v[i] + 0; for (j = i - 1; j >= 1 && v[j] + 0 > t; j--) v[j + 1] = v[j]; v[j + 1] = t }
      lo = v[1]; hi = v[n]; return n % 2 ? v[(n + 1) / 2] : (v[n / 2] + v[n / 2 + 1]) / 2 }
    $2 != "tools" { next }
    { k = $3 SUBSEP $4 SUBSEP $5; if (!(k in seen)) { seen[k] = 1; ord[++n] = k; p[k] = $3; sz[k] = $4; tl[k] = $5 } tot[k]++ }
    $16 == "oom-kill" || $16 == "timeout" || $16 == "skipped" { bad[k] = bad[k] (bad[k] == "" ? "" : ", ") $16; next }
    $22 == "no result" { bad[k] = bad[k] (bad[k] == "" ? "" : ", ") "no result (exit " $13 ")"; next }
    { ok[k]++; w[k] = w[k] " " $9; r[k] = r[k] " " $12; hp[k] = $14; out[k] = $15; cnt[k] = ($22 != "" ? $22 : ($19 != "" ? $19 " lines" : "")) }
    END { for (i = 1; i <= n; i++) { k = ord[i]
      t = ""; f = ""; s = ""; m = ""
      if (ok[k]) { t = sprintf("%.2f s", med(w[k])); f = sprintf("%.2f s", lo); s = sprintf("%.2f s", hi); m = h(med(r[k]) * 1024) }
      printf "| %s | %s | %s | %d of %d | %s | %s | %s | %s | %s | %s | %s | %s |\n", p[k], h(sz[k]), tl[k], ok[k], tot[k], t, f, s, m, h(hp[k]), h(out[k]), cnt[k], bad[k] } }'
  fi
  if awk -F'\t' '$2 == "le" { f = 1 } END { exit !f }' "$runs"; then
    echo
    echo "## Low-entropy runs"
    echo
    echo "One row per pair. rift is \`rift --stats --mem 256M\`; peak heap is exact; approx share is the lines of A and B in approximate hunks over all lines of A and B; changed lines are the - and + lines of rift's diff and the < and > lines of GNU diff's; round trip is ok when patch applied rift's diff to A and the result matched B byte for byte."
    echo
    echo "| pair | size | rift wall | peak heap | approx share | diff | rift changed lines | GNU diff changed lines | round trip | rift result |"
    echo "|---|---|---|---|---|---|---|---|---|---|"
    awk -F'\t' '
    function h(b,   u, i) { if (b == "") return ""; split("B KiB MiB GiB TiB", u, " "); i = 1; while (b >= 1024 && i < 5) { b /= 1024; i++ } return sprintf(i == 1 ? "%d %s" : "%.1f %s", b, u[i]) }
    $2 != "le" { next }
    { k = $3 SUBSEP $4; if (!(k in seen)) { seen[k] = 1; ord[++n] = k; pair[k] = $3; size[k] = $4 } }
    $5 ~ /^rift/ { wall[k] = $9; heap[k] = $14; out[k] = $15; res[k] = $16 " " $13
      share[k] = ($17 != "" && $18 > 0) ? sprintf("%.4f", $17 / $18) : "" }
    $5 ~ /^patch/ { rch[k] = $19; rt[k] = $20 }
    $5 ~ /^diff/ { gch[k] = ($16 == "success" || $16 == "exit-code" || $16 == "n/a") ? $19 : $16 }
    END { for (i = 1; i <= n; i++) { k = ord[i]
      printf "| %s | %s | %s | %s | %s | %s | %s | %s | %s | %s |\n", pair[k], h(size[k]), wall[k], h(heap[k]), share[k], h(out[k]), rch[k], gch[k], rt[k], res[k] } }' "$runs"
  fi
} > "$pkg/report.md"

name=rift-bench-$label
archive=$outdir/$name.tar.gz
[ ! -e "$archive" ] || archive=$outdir/$name-$$.tar.gz
tar -C "$pkg" -czf "$archive" report.md results.json raw
echo "Done: $archive" >&2
