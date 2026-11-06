#!/bin/sh
# Fuzzing for a Linux host: no root, nothing written to disk except the
# result archive in the current directory.
#
#   fuzz/run-linux.sh [--dry-run] [--yes] [--minutes N] [--ref REF]
#                     [--repo URL]
#
#   --dry-run    print the plan and exit
#   --yes        skip the 10 second wait before the run starts
#   --minutes N  fuzzing time per target (default 30)
#   --ref REF    commit, branch or tag to build (default HEAD)
#   --repo URL   where to clone from (default the repository this script
#                is in; uncommitted changes are not part of the run)
#
# Test knobs, for running the script on a host that is not Linux. They turn
# safety checks off; never set them for a real run:
#   RIFT_FUZZ_TESTING=1    skip the Linux, tmpfs and toolchain checks
#   RIFT_FUZZ_MEMINFO=F    read MemAvailable from F instead of /proc/meminfo
#   RIFT_FUZZ_SHM=DIR      use DIR instead of /dev/shm
#   RIFT_FUZZ_LIMITER=L    force systemd, prlimit or none (none runs uncapped)
#   RIFT_FUZZ_BINS=DIR     skip the clone and build: run the executables in
#                          DIR (named after the targets) on the corpora of
#                          this checkout
#
# See fuzz/README.md.
set -eu

GIB=1073741824
MIB=1048576
schema=1
targets="scan keyed_csv keyed_ndjson keyed_copy keyed_reader pipeline unified tui_render sanitize mcp_request mcp_tools"
# Per fuzzing process: libFuzzer's own RSS limit, and the cap around it.
rss_mb=2048
child=$((rss_mb * MIB + GIB / 2))
# The fork-mode parent, the margin left to the host, and the tmpfs the
# binaries and corpora take once the build is gone.
parent=$((GIB / 2)) margin=$((2 * GIB)) kept=$GIB
# The build: tmpfs for the clone, crates and target directory, and the
# RSS of up to four compilers.
build_tmpfs=$((3 * GIB)) build_rss=$((3 * GIB)) build_jobs=4

self=$(cd "$(dirname "$0")/.." && pwd)
repo=$self ref=HEAD yes=0 dry=0 minutes=30
testing=${RIFT_FUZZ_TESTING:-0}
meminfo=${RIFT_FUZZ_MEMINFO:-/proc/meminfo}
bins=${RIFT_FUZZ_BINS:-}
outdir=$(pwd)

die() { echo "run-linux: $*" >&2; exit 1; }
usage() { sed -n '2,/^set/p' "$0" | sed '$d' >&2; exit 2; }

while [ $# -gt 0 ]; do
  case $1 in
    --dry-run) dry=1; shift ;;
    --yes) yes=1; shift ;;
    --minutes) [ $# -ge 2 ] || usage; minutes=$2; shift 2 ;;
    --ref) [ $# -ge 2 ] || usage; ref=$2; shift 2 ;;
    --repo) [ $# -ge 2 ] || usage; repo=$2; shift 2 ;;
    *) usage ;;
  esac
done
case $minutes in
  ''|*[!0-9]*|0*) die "--minutes takes a whole number above zero" ;;
esac

# ---- checks that need no scratch directory ---------------------------------

[ "$(id -u)" -ne 0 ] || die "refusing to run as root; nothing here needs it"
# Never let rustup install a toolchain behind our back, not even for a check.
export RUSTUP_AUTO_INSTALL=0
if [ "$testing" != 1 ]; then
  [ "$(uname -s)" = Linux ] || die "Linux only"
  command -v cargo >/dev/null 2>&1 ||
    die "cargo not found. Install a Rust toolchain with rustup first; this script does not install one."
  nightly=$(cargo +nightly --version 2>/dev/null) ||
    die "no nightly toolchain; install it with: rustup toolchain install nightly --profile minimal"
  command -v git >/dev/null 2>&1 || die "git not found"
else
  nightly=$(cargo +nightly --version 2>/dev/null || echo "cargo nightly (not checked)")
fi

is_tmpfs() { [ "$testing" = 1 ] || [ "$(stat -f -c %T "$1" 2>/dev/null)" = tmpfs ]; }
free_bytes() { df -Pk "$1" | awk 'NR==2 {print $4 * 1024}'; }

# ---- scratch location: tmpfs only ------------------------------------------

base=
for d in "${RIFT_FUZZ_SHM:-/dev/shm}" "${XDG_RUNTIME_DIR:-}"; do
  if [ -n "$d" ] && [ -d "$d" ] && [ -w "$d" ] && is_tmpfs "$d" &&
     [ "$(free_bytes "$d")" -ge "$build_tmpfs" ]; then
    base=$d
    break
  fi
done
[ -n "$base" ] || die "no writable tmpfs with $((build_tmpfs / GIB)) GiB free (/dev/shm, \$XDG_RUNTIME_DIR); refusing to touch the disk"
tmpfs_free=$(free_bytes "$base")

# ---- memory limiter --------------------------------------------------------

limiter=none
if command -v systemd-run >/dev/null 2>&1 &&
   systemd-run --user --scope -q -p MemoryMax=64M true >/dev/null 2>&1; then
  limiter=systemd
elif command -v prlimit >/dev/null 2>&1; then
  limiter=prlimit
fi
[ "$testing" != 1 ] || limiter=${RIFT_FUZZ_LIMITER:-$limiter}
[ "$limiter" != none ] || [ "$testing" = 1 ] || die "no memory cap available (no systemd user scope, no prlimit); refusing to fuzz without one"
# AddressSanitizer reserves terabytes of address space, which an
# address-space limit cannot allow: under prlimit the targets are built
# without it.
if [ "$limiter" = systemd ]; then sanitizer=address; else sanitizer=none; fi
niceio="nice -n 10"
command -v ionice >/dev/null 2>&1 && niceio="$niceio ionice -c3"

# ---- sizing ----------------------------------------------------------------

avail=$(awk '/^MemAvailable:/ {print $2 * 1024}' "$meminfo" 2>/dev/null || true)
[ -n "$avail" ] || die "no MemAvailable in $meminfo"
limit=$((avail * 60 / 100))
mem_total=$(awk '/^MemTotal:/ {print $2 * 1024}' "$meminfo")
cpus=$(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN)

need_build=$((build_tmpfs + build_rss + margin))
[ "$need_build" -le "$limit" ] ||
  die "refusing to start: the build needs $((need_build * 100 / 60 / MIB)) MiB of MemAvailable (60% rule), this host has $((avail / MIB)) MiB"
# Fork-mode workers: as many as fit the 60% budget, leaving one CPU free.
workers=$(( (limit - kept - parent - margin) / child ))
[ "$workers" -le $((cpus - 1)) ] || workers=$((cpus - 1))
[ "$workers" -ge 1 ] || workers=1
[ $((kept + parent + margin + workers * child)) -le "$limit" ] ||
  die "refusing to start: one fuzzing process needs $(( (kept + parent + margin + child) * 100 / 60 / MIB)) MiB of MemAvailable (60% rule), this host has $((avail / MIB)) MiB"
cap=$((parent + workers * child))
ntargets=$(echo "$targets" | wc -w | tr -d ' ')

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
  # Never a local path: it would name the user.
  if [ -n "$bins" ]; then
    echo "  source            prebuilt executables (test knob), corpora of this checkout"
  elif [ "$repo" = "$self" ]; then
    echo "  source            the checkout this script is in, at $ref"
  else
    echo "  source            $repo at $ref"
  fi
  echo "  toolchain         $nightly (never installed by this script)"
  echo "  build             cargo fuzz build -O -s $sanitizer, $build_jobs jobs; then only the binaries are kept"
  echo "  targets           $targets"
  echo "  per target        $minutes min, libFuzzer -fork=$workers, -rss_limit_mb=$rss_mb, -timeout=10"
  case $limiter in
    systemd) echo "  memory cap        systemd-run --user scope per target, MemoryMax=$(gib "$cap"), MemorySwapMax=0" ;;
    prlimit) echo "  memory cap        prlimit --as=$(gib "$child") per process (no systemd user scope): address space, not RSS; no AddressSanitizer" ;;
    none) echo "  memory cap        none (test knobs only)" ;;
  esac
  echo "  priority          $niceio"
  echo "  total time        $((ntargets * minutes)) min of fuzzing ($ntargets targets x $minutes min), plus the build"
  echo "  output            rift-fuzz-$label.tar.gz in the current directory"
}

plan
[ "$dry" = 0 ] || exit 0

if [ "$yes" != 1 ]; then
  echo "Starting in 10 s. Ctrl-C to cancel." >&2
  sleep 10
fi

# ---- scratch directory and cleanup -----------------------------------------

scratch=$base/rift-fuzz-$$
unit='' fpid=''
cleanup() {
  # Stop a fuzzer still running when the script is interrupted.
  if [ -n "$unit" ]; then
    systemctl --user stop "$unit.scope" >/dev/null 2>&1 || true
  elif [ -n "$fpid" ]; then
    pkill -TERM -P "$fpid" 2>/dev/null || true
    kill -TERM "$fpid" 2>/dev/null || true
  fi
  case $scratch in
    */rift-fuzz-*) rm -rf "$scratch" ;;
  esac
}
# Arm the cleanup only once the directory is ours: if it already exists,
# mkdir fails and the existing directory is left alone.
mkdir -m 700 "$scratch" || die "$scratch already exists; remove it or rerun"
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

pkg=$scratch/pkg
mkdir -p "$pkg/artifacts" "$pkg/logs" "$scratch/bin" "$scratch/corpus" "$scratch/tmp"
facts=$scratch/facts.tsv runs=$scratch/runs.tsv notes=$scratch/notes.txt
: > "$facts"; : > "$runs"; : > "$notes"
fact() { printf '%s\t%s\n' "$1" "$2" >> "$facts"; }
note() { echo "$*" >> "$notes"; echo "note: $*" >&2; }
loadavg() { cut -d' ' -f1-3 /proc/loadavg 2>/dev/null || echo n/a; }
# libFuzzer's fork mode and the mcp target put their files in TMPDIR.
export TMPDIR="$scratch/tmp"

# ---- build -----------------------------------------------------------------

if [ -n "$bins" ]; then
  for t in $targets; do cp "$bins/$t" "$scratch/bin/$t"; done
  src=$self commit=$(git -C "$self" rev-parse HEAD 2>/dev/null || echo unknown)
  cp -R "$self/fuzz/corpus/." "$scratch/corpus/"
else
  git clone -q "$repo" "$scratch/src" || die "clone of $repo failed"
  git -C "$scratch/src" checkout -q "$ref" || die "ref $ref not found in $repo"
  commit=$(git -C "$scratch/src" rev-parse HEAD)
  src=$scratch/src
  [ -d "$src/fuzz/corpus" ] || die "$ref has no fuzz/ directory"
  cp -R "$src/fuzz/corpus/." "$scratch/corpus/"
  # Everything cargo writes goes to the scratch directory.
  export CARGO_HOME="$scratch/cargo" CARGO_TARGET_DIR="$scratch/target" CARGO_BUILD_JOBS=$build_jobs
  if ! command -v cargo-fuzz >/dev/null 2>&1; then
    echo "Building cargo-fuzz into $CARGO_HOME" >&2
    # shellcheck disable=SC2086
    $niceio cargo +nightly install -q --locked cargo-fuzz --version 0.13.2 >"$pkg/logs/build.log" 2>&1 ||
      { tail -n 20 "$pkg/logs/build.log" >&2; die "cargo-fuzz build failed"; }
    PATH=$CARGO_HOME/bin:$PATH
  fi
  echo "Building the fuzz targets of $ref ($commit) with $nightly" >&2
  # shellcheck disable=SC2086
  (cd "$src" && $niceio cargo +nightly fuzz build -O -s "$sanitizer" --target-dir "$CARGO_TARGET_DIR") >>"$pkg/logs/build.log" 2>&1 ||
    { tail -n 20 "$pkg/logs/build.log" >&2; die "build failed"; }
  for t in $targets; do
    for f in "$CARGO_TARGET_DIR"/*/release/"$t"; do
      cp "$f" "$scratch/bin/$t" || die "no binary for $t"
    done
  done
  rm -rf "$CARGO_TARGET_DIR" "$CARGO_HOME"
fi

# ---- host facts ------------------------------------------------------------

fact tool "fuzz/run-linux.sh schema $schema"
fact host "$label"
fact date "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
fact cpu_model "$cpu_model"
fact cpu_threads "$cpus"
fact mem_total_bytes "$mem_total"
fact mem_available_bytes "$avail"
fact kernel "$(uname -r)"
# shellcheck disable=SC1091
fact distro "$(. /etc/os-release 2>/dev/null && echo "${PRETTY_NAME:-unknown}" || echo unknown)"
fact virt "$(systemd-detect-virt 2>/dev/null || echo unknown)"
fact toolchain "$nightly"
fact rift_ref "$ref"
fact rift_commit "$commit"
fact sanitizer "$sanitizer"
fact limiter "$limiter"
fact workers "$workers"
fact minutes_per_target "$minutes"
fact load_before "$(loadavg)"

# ---- fuzzing ---------------------------------------------------------------

# corpus_stats DIR: "files bytes"
corpus_stats() {
  find "$1" -type f | awk '{n++} END {print n + 0}' | tr '\n' ' '
  find "$1" -type f -exec cat {} + 2>/dev/null | wc -c | tr -d ' '
}

# scope_result UNIT: systemd's Result for the scope, as in bench/linux-safe.sh.
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

secs=$((minutes * 60))
for t in $targets; do
  corpus=$scratch/corpus/$t art=$pkg/artifacts/$t log=$pkg/logs/$t.log
  mkdir -p "$corpus" "$art"
  before=$(corpus_stats "$corpus")
  echo "Fuzzing $t for $minutes min" >&2
  unit=
  case $limiter in
    systemd)
      unit=rift-fuzz-$$-$t
      lim="systemd-run --user --scope -q --unit=$unit -p MemoryMax=$cap -p MemorySwapMax=0 $niceio" ;;
    prlimit) lim="prlimit --as=$child $niceio" ;;
    *) lim=$niceio ;;
  esac
  start=$(date +%s)
  # Leaks are not reported: memory that grows is caught by the RSS limit,
  # and lazily built statics would read as leaks. $lim is a word list.
  # shellcheck disable=SC2086
  ASAN_OPTIONS=detect_leaks=0 MALLOC_ARENA_MAX=2 RUST_BACKTRACE=1 \
    $lim "$scratch/bin/$t" -fork="$workers" -ignore_crashes=1 -ignore_timeouts=1 -ignore_ooms=1 \
    -max_total_time="$secs" -rss_limit_mb="$rss_mb" -timeout=10 -detect_leaks=0 \
    -artifact_prefix="$art/" "$corpus" > "$log" 2>&1 &
  fpid=$!
  ec=0
  wait "$fpid" || ec=$?
  fpid=
  end=$(date +%s)
  result=$(scope_result "$unit")
  unit=
  after=$(corpus_stats "$corpus")
  # The last fork-mode status line:
  # #RUNS: cov: C ft: F corp: N exec/s: E oom/timeout/crash: O/T/C time: Ss ...
  stats=$(awk '/^#[0-9]+: cov: / {
      runs = substr($1, 2, length($1) - 2); cov = $3; ft = $5; eps = $9
      split($11, o, "/"); oom = o[1]; to = o[2]; cr = o[3] }
    END { print runs "\t" cov "\t" ft "\t" eps "\t" oom "\t" to "\t" cr }' "$log")
  nart=$(find "$art" -type f | wc -l | tr -d ' ')
  # Keep logs bounded; a crash's stack trace is near its first report.
  if [ "$(wc -c < "$log")" -gt $((8 * MIB)) ]; then
    head -c $((8 * MIB)) "$log" > "$log.head" && mv "$log.head" "$log"
    note "$t: log cut to its first 8 MiB"
  fi
  [ "$ec" = 0 ] || note "$t: libFuzzer exited with $ec"
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$t" "$((end - start))" "$stats" "${before% *}" "${before#* }" "${after% *}" "${after#* }" "$nart" "$ec" "$result" >> "$runs"
done

fact load_after "$(loadavg)"
[ "$limiter" != prlimit ] || note "no systemd user scope: caps are prlimit --as address-space limits and the targets ran without AddressSanitizer"

# ---- report ----------------------------------------------------------------

# runs.tsv: target seconds runs cov ft exec/s ooms timeouts crashes
# files_before bytes_before files_after bytes_after artifacts exit result
{ awk '{print "F\t" $0}' "$facts"; awk '{print "N\t" $0}' "$notes"; awk '{print "R\t" $0}' "$runs"; } | awk -F'\t' -v schema="$schema" '
function esc(s) { gsub(/\\/, "\\\\", s); gsub(/"/, "\\\"", s); gsub(/[\001-\037]/, " ", s); return s }
function num(x) { return x == "" ? "null" : x }
$1 == "F" { f[++nf] = $2; v[nf] = $3; next }
$1 == "N" { nt[++nn] = $2; next }
{
  r[++nr] = sprintf("    {\"target\": \"%s\", \"seconds\": %s, \"executions\": %s, \"exec_per_s\": %s, \"coverage\": %s, \"features\": %s, \"corpus_files_before\": %s, \"corpus_bytes_before\": %s, \"corpus_files_after\": %s, \"corpus_bytes_after\": %s, \"crashes\": %s, \"timeouts\": %s, \"ooms\": %s, \"artifacts\": %s, \"exit_code\": %s, \"result\": \"%s\"}",
    $2, num($3), num($4), num($7), num($5), num($6), num($11), num($12), num($13), num($14), num($10), num($9), num($8), num($15), num($16), $17)
}
END {
  print "{\n  \"schema_version\": " schema ",\n  \"host\": {"
  for (i = 1; i <= nf; i++) printf "    \"%s\": \"%s\"%s\n", f[i], esc(v[i]), (i < nf ? "," : "")
  print "  },\n  \"notes\": ["
  for (i = 1; i <= nn; i++) printf "    \"%s\"%s\n", esc(nt[i]), (i < nn ? "," : "")
  print "  ],\n  \"targets\": ["
  for (i = 1; i <= nr; i++) print r[i] (i < nr ? "," : "")
  print "  ]\n}"
}' > "$pkg/results.json"

{
  echo "Fuzzing of rift with cargo-fuzz (libFuzzer fork mode). Inputs that crashed, timed out or ran out of memory are under artifacts/<target>/, the libFuzzer output under logs/."
  echo
  echo "## Host"
  echo
  awk -F'\t' '{printf "- %s: %s\n", $1, $2}' "$facts"
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
  echo "## Targets"
  echo
  echo "Executions, coverage (libFuzzer's cov) and features (ft) are from the last status line. Corpus is files / bytes. Result is systemd's verdict on the target's scope (oom-kill means the cap killed it; n/a without systemd)."
  echo
  echo "| target | seconds | executions | exec/s | cov | ft | corpus before | corpus after | crashes | timeouts | OOMs | artifacts | exit | result |"
  echo "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
  awk -F'\t' '{ printf "| %s | %s | %s | %s | %s | %s | %s / %s | %s / %s | %s | %s | %s | %s | %s | %s |\n",
      $1, $2, $3, $6, $4, $5, $10, $11, $12, $13, $9, $8, $7, $14, $15, $16 }' "$runs"
} > "$pkg/report.md"

name=rift-fuzz-$label
archive=$outdir/$name.tar.gz
[ ! -e "$archive" ] || archive=$outdir/$name-$$.tar.gz
tar -C "$pkg" -czf "$archive" report.md results.json artifacts logs
echo "Done: $archive" >&2
