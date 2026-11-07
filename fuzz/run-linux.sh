#!/bin/sh
# Fuzzing for a Linux host: no root, nothing written to disk except the
# result archive in the current directory.
#
#   fuzz/run-linux.sh [--dry-run] [--yes] [--time S] [--jobs N] [--ref REF]
#                     [--repo URL]
#
#   --dry-run    print the plan and exit
#   --yes        skip the 10 second wait before the run starts
#   --time S     fuzzing time per target in seconds (default 1800);
#                --minutes N is the same as --time $((N * 60))
#   --jobs N     targets fuzzed at once (default: physical cores minus 2,
#                at least 1). Each one is pinned to its own cores and gets
#                its own memory cap; N is lowered when they do not fit in
#                60% of MemAvailable
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
#   RIFT_FUZZ_ALLOWED=L    the allowed CPUs as a list, "0-11" or "0,2-5", instead
#                          of Cpus_allowed_list in /proc/self/status
#   RIFT_FUZZ_TOPO=F       read "cpu,core,socket" lines from F instead of
#                          lscpu -p=cpu,core,socket
#   RIFT_FUZZ_TASKSET_LOG=F  use a stand-in for taskset that appends its
#                          arguments to F and then runs the command unpinned
#   RIFT_FUZZ_BINS=DIR     skip the clone and build: run the executables in
#                          DIR (named after the targets) on the corpora of
#                          this checkout
#
# See fuzz/README.md.
set -eu

GIB=1073741824
MIB=1048576
schema=3
targets="scan keyed_csv keyed_ndjson keyed_copy keyed_reader pipeline unified tui_render sanitize mcp_request mcp_tools"
# Per fuzzing process: libFuzzer's own RSS limit, and the cap around it.
# rss_mb is a cap, not a measurement: it sizes the slots, the scope's
# MemoryMax and -rss_limit_mb alike. Tune it from the memory_peak_bytes
# of a real run (results.json), not from this number.
rss_mb=1024
child=$((rss_mb * MIB + GIB / 2))
# The fork-mode parent, the margin left to the host, and the tmpfs the
# binaries and corpora take once the build is gone.
parent=$((GIB / 2)) margin=$((2 * GIB)) kept=$GIB
# The build: tmpfs for the clone, crates and target directory, and the RSS
# of the compilers, about 0.75 GiB each; 4 at once when memory allows.
build_tmpfs=$((3 * GIB)) job_rss=$((3 * GIB / 4)) build_jobs=4

self=$(cd "$(dirname "$0")/.." && pwd)
repo=$self ref=HEAD yes=0 dry=0 secs=1800 jobs=
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
    --time) [ $# -ge 2 ] || usage; secs=$2; shift 2 ;;
    --minutes) [ $# -ge 2 ] || usage; case $2 in ''|*[!0-9]*) die "--minutes takes a whole number above zero" ;; esac; secs=$(($2 * 60)); shift 2 ;;
    --jobs) [ $# -ge 2 ] || usage; jobs=$2; shift 2 ;;
    --ref) [ $# -ge 2 ] || usage; ref=$2; shift 2 ;;
    --repo) [ $# -ge 2 ] || usage; repo=$2; shift 2 ;;
    *) usage ;;
  esac
done
case $secs in
  ''|*[!0-9]*|0*) die "--time takes a whole number of seconds above zero" ;;
esac
case $jobs in
  *[!0-9]*|0*) die "--jobs takes a whole number above zero" ;;
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

# Fewer compilers at once when memory is short: slower, not larger.
for j in 4 2 1; do
  build_jobs=$j
  need_build=$((build_tmpfs + j * job_rss + margin))
  [ "$need_build" -gt "$limit" ] || break
done
[ "$need_build" -le "$limit" ] ||
  die "refusing to start: the build needs $((need_build * 100 / 60 / MIB)) MiB of MemAvailable even with one compiler (60% rule), this host has $((avail / MIB)) MiB"

# ---- cores -----------------------------------------------------------------

# The CPUs this process may use, then one line per physical core listing
# its hardware threads (so siblings stay in one slot), grouped by socket.
allowed_spec=${RIFT_FUZZ_ALLOWED:-$(awk '/^Cpus_allowed_list:/ {print $2}' /proc/self/status 2>/dev/null || true)}
[ -n "$allowed_spec" ] || allowed_spec="0-$(( $(nproc 2>/dev/null || getconf _NPROCESSORS_ONLN) - 1 ))"
allowed=$(echo "$allowed_spec" | awk -F, '{ for (i = 1; i <= NF; i++) { n = split($i, r, "-"); if (n == 1) r[2] = r[1]; for (c = r[1]; c <= r[2]; c++) printf "%d ", c } }')
if [ -n "${RIFT_FUZZ_TOPO:-}" ]; then
  topo=$(cat "$RIFT_FUZZ_TOPO")
else
  topo=$(lscpu -p=cpu,core,socket 2>/dev/null || true)
fi
cores=$(echo "$topo" | awk -F, -v a="$allowed" '
  BEGIN { n = split(a, x, " "); for (i = 1; i <= n; i++) ok[x[i]] = 1 }
  /^#/ || NF < 3 { next }
  ($1 in ok) { k = $3 "-" $2; if (!(k in c)) { order[++m] = k; sock[k] = $3 }
               c[k] = c[k] (c[k] == "" ? "" : ",") $1; seen[$1] = 1 }
  END { for (i = 1; i <= n; i++) if (!(x[i] in seen)) { k = "c" x[i]; order[++m] = k; sock[k] = 0; c[k] = x[i] }
        for (i = 1; i <= m; i++) print sock[order[i]] "\t" c[order[i]] }' | sort -s -n -k1,1 | cut -f2)
phys=$(echo "$cores" | wc -l | tr -d ' ')
cpus=$(echo "$allowed" | wc -w | tr -d ' ')
[ "$phys" -ge 1 ] || die "no usable CPU found"
ntargets=$(echo "$targets" | wc -w | tr -d ' ')

# ---- jobs and memory -------------------------------------------------------

# Every running target is one slot: a systemd scope with MemoryMax=cap,
# holding a fork-mode parent and w workers. Fixed: tmpfs and the margin.
jobs_note=
if [ -z "$jobs" ]; then
  jobs=$((phys - 2)); [ "$jobs" -ge 1 ] || jobs=1
  jobs_how="default: $phys physical cores minus 2"
else
  jobs_how="--jobs"
fi
[ "$jobs" -le "$phys" ] || { jobs_note="--jobs $jobs lowered to $phys, the number of physical cores"; jobs=$phys; }
[ "$jobs" -le "$ntargets" ] || jobs=$ntargets
fit=$(( (limit - kept - margin) / (parent + child) ))
[ "$fit" -ge 1 ] ||
  die "refusing to start: one fuzzing process needs $(( (kept + parent + margin + child) * 100 / 60 / MIB)) MiB of MemAvailable (60% rule), this host has $((avail / MIB)) MiB"
if [ "$jobs" -gt "$fit" ]; then
  jobs_note="jobs lowered from $jobs to $fit: $jobs targets at $(( (parent + child) / MIB )) MiB each would not fit in 60% of MemAvailable"
  jobs=$fit
fi
slots=$(echo "$cores" | awk -v n="$jobs" '
  { c[NR] = $0 }
  END { base = int(NR / n); extra = NR % n; k = 0
        for (s = 1; s <= n; s++) { m = base + (s <= extra); l = ""
          for (j = 0; j < m; j++) { k++; l = l (l == "" ? "" : ",") c[k] }
          print l } }')
# Fork workers per target: the CPUs of the smallest slot, as many as the
# memory left after the other slots allows.
min_cpus=$(echo "$slots" | awk -F, 'NR == 1 || NF < m { m = NF } END { print m }')
mem_workers=$(( ((limit - kept - margin) / jobs - parent) / child ))
workers=$min_cpus
[ "$workers" -le "$mem_workers" ] || workers=$mem_workers
[ "$workers" -ge 1 ] || workers=1
cap=$((parent + workers * child))
rounds=$(( (ntargets + jobs - 1) / jobs ))
wall=$((rounds * secs))
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
  echo "  build             cargo fuzz build -O -s $sanitizer, compilers at once: $build_jobs; then only the binaries are kept"
  echo "  targets           $targets"
  echo "  jobs              $jobs targets at once ($jobs_how; $phys physical cores, $cpus threads)"
  [ -z "$jobs_note" ] || echo "                    note: $jobs_note"
  i=0
  echo "$slots" | while IFS= read -r l; do
    i=$((i + 1)); echo "  slot $i            cores $l"
  done
  echo "  per target        $secs s, libFuzzer -fork=$workers, -rss_limit_mb=$rss_mb, -timeout=10"
  case $limiter in
    systemd) echo "  memory cap        systemd-run --user scope per target, MemoryMax=$(gib "$cap") each ($(gib $((cap * jobs))) for $jobs), MemorySwapMax=0" ;;
    prlimit) echo "  memory cap        prlimit --as=$(gib "$child") per process, $jobs targets (no systemd user scope): address space, not RSS; no AddressSanitizer" ;;
    none) echo "  memory cap        none (test knobs only)" ;;
  esac
  echo "  priority          $niceio, taskset -c <slot cores>"
  echo "  total time        $((wall / 60)) min $((wall % 60)) s of fuzzing ($ntargets targets in $rounds rounds of $secs s), plus the build"
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
spids=''
cleanup() {
  # Stop the slot loops first so none starts another target, then the
  # fuzzers they were running (and, in fork mode, their workers).
  kids=
  for sp in $spids; do kids="$kids $(pgrep -P "$sp" 2>/dev/null || true)"; done
  for sp in $spids; do kill -TERM "$sp" 2>/dev/null || true; done
  for t in $targets; do
    systemctl --user stop "rift-fuzz-$$-$t.scope" >/dev/null 2>&1 || true
  done
  for c in $kids; do
    pkill -TERM -P "$c" 2>/dev/null || true
    kill -TERM "$c" 2>/dev/null || true
  done
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
mkdir -p "$pkg/artifacts" "$pkg/logs" "$scratch/bin" "$scratch/corpus" "$scratch/tmp" "$scratch/runs.d"
facts=$scratch/facts.tsv runs=$scratch/runs.tsv notes=$scratch/notes.txt
: > "$facts"; : > "$runs"; : > "$notes"
fact() { printf '%s\t%s\n' "$1" "$2" >> "$facts"; }
note() { echo "$*" >> "$notes"; echo "note: $*" >&2; }
loadavg() { cut -d' ' -f1-3 /proc/loadavg 2>/dev/null || echo n/a; }
# libFuzzer's fork mode and the mcp target put their files in TMPDIR.
export TMPDIR="$scratch/tmp"

# taskset pins a target to its slot's cores. Without util-linux's taskset
# the real run stops; the test stand-in logs its arguments and runs unpinned.
if [ -n "${RIFT_FUZZ_TASKSET_LOG:-}" ] && [ "$testing" = 1 ]; then
  mkdir "$scratch/shim"
  printf '#!/bin/sh\necho "$*" >> "%s"\nshift 2\nexec "$@"\n' "$RIFT_FUZZ_TASKSET_LOG" > "$scratch/shim/taskset"
  chmod +x "$scratch/shim/taskset"
  PATH=$scratch/shim:$PATH
fi
command -v taskset >/dev/null 2>&1 || die "taskset not found (util-linux); refusing to run without core pinning"

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
fact jobs "$jobs"
fact workers_per_target "$workers"
fact seconds_per_target "$secs"
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

# run_target T CORES: one target, to completion; its row goes to runs.d/T.
run_target() {
  t=$1 cores_list=$2
  corpus=$scratch/corpus/$t art=$pkg/artifacts/$t log=$pkg/logs/$t.log
  mkdir -p "$corpus" "$art"
  before=$(corpus_stats "$corpus")
  echo "Fuzzing $t for $secs s on cores $cores_list" >&2
  unit=
  case $limiter in
    systemd)
      unit=rift-fuzz-$$-$t
      lim="systemd-run --user --scope -q --unit=$unit -p MemoryMax=$cap -p MemorySwapMax=0 $niceio" ;;
    prlimit) lim="prlimit --as=$child $niceio" ;;
    *) lim=$niceio ;;
  esac
  start=$(date +%s)
  rm -f "$scratch/runs.d/$t.peak"
  # Leaks are not reported: memory that grows is caught by the RSS limit,
  # and lazily built statics would read as leaks. $lim is a word list.
  # shellcheck disable=SC2086
  ASAN_OPTIONS=detect_leaks=0 MALLOC_ARENA_MAX=2 RUST_BACKTRACE=1 \
    $lim taskset -c "$cores_list" "$scratch/bin/$t" -fork="$workers" -ignore_crashes=1 -ignore_timeouts=1 -ignore_ooms=1 \
    -max_total_time="$secs" -rss_limit_mb="$rss_mb" -timeout=10 -detect_leaks=0 \
    -artifact_prefix="$art/" "$corpus" > "$log" 2>&1 &
  fpid=$!
  # A successful scope is collected the moment its last process exits, and
  # with it MemoryPeak, so the figure is sampled while the scope exists. The
  # peak only grows: the last good sample is the answer, short of what the
  # final second added. Anything that is not a plain number (property
  # missing, "[not set]", infinity) is not recorded.
  sampler=
  if [ -n "$unit" ]; then
    (
      while kill -0 "$fpid" 2>/dev/null; do
        p=$(systemctl --user show "$unit.scope" -p MemoryPeak --value 2>/dev/null || true)
        case $p in
          ''|*[!0-9]*) ;;
          *) [ "${#p}" -gt 18 ] || echo "$p" > "$scratch/runs.d/$t.peak" ;;
        esac
        sleep 1
      done
    ) &
    sampler=$!
  fi
  ec=0
  wait "$fpid" || ec=$?
  [ -z "$sampler" ] || wait "$sampler" || true
  peak=$(cat "$scratch/runs.d/$t.peak" 2>/dev/null || true)
  end=$(date +%s)
  result=$(scope_result "$unit")
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
  printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n' \
    "$t" "$((end - start))" "$stats" "${before% *}" "${before#* }" "${after% *}" "${after#* }" "$nart" "$ec" "$result" "${peak:-unknown}" > "$scratch/runs.d/$t"
}

# Target i goes to slot i mod jobs and the slot's targets run one after the
# other; all last the same time, so a fixed assignment keeps every slot busy.
s=0
for sl in $slots; do
  s=$((s + 1))
  (
    i=0
    for t in $targets; do
      [ $((i % jobs + 1)) = "$s" ] && run_target "$t" "$sl"
      i=$((i + 1))
    done
  ) &
  spids="$spids $!"
done
for sp in $spids; do wait "$sp" || true; done
spids=
for t in $targets; do cat "$scratch/runs.d/$t" >> "$runs" 2>/dev/null || note "$t: no result"; done

fact load_after "$(loadavg)"
[ "$limiter" != prlimit ] || note "no systemd user scope: caps are prlimit --as address-space limits and the targets ran without AddressSanitizer"

# ---- report ----------------------------------------------------------------

# runs.tsv: target seconds runs cov ft exec/s ooms timeouts crashes
# files_before bytes_before files_after bytes_after artifacts exit result
# memory_peak (bytes or unknown)
{ awk '{print "F\t" $0}' "$facts"; awk '{print "N\t" $0}' "$notes"; awk '{print "R\t" $0}' "$runs"; } | awk -F'\t' -v schema="$schema" '
function esc(s) { gsub(/\\/, "\\\\", s); gsub(/"/, "\\\"", s); gsub(/[\001-\037]/, " ", s); return s }
function num(x) { return x == "" ? "null" : x }
$1 == "F" { f[++nf] = $2; v[nf] = $3; next }
$1 == "N" { nt[++nn] = $2; next }
{
  r[++nr] = sprintf("    {\"target\": \"%s\", \"seconds\": %s, \"executions\": %s, \"exec_per_s\": %s, \"coverage\": %s, \"features\": %s, \"corpus_files_before\": %s, \"corpus_bytes_before\": %s, \"corpus_files_after\": %s, \"corpus_bytes_after\": %s, \"crashes\": %s, \"timeouts\": %s, \"ooms\": %s, \"artifacts\": %s, \"exit_code\": %s, \"result\": \"%s\", \"memory_peak_bytes\": %s}",
    $2, num($3), num($4), num($7), num($5), num($6), num($11), num($12), num($13), num($14), num($10), num($9), num($8), num($15), num($16), $17, ($18 ~ /^[0-9]+$/ ? $18 : "null"))
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
  echo "Executions, coverage (libFuzzer's cov) and features (ft) are from the last status line. Corpus is files / bytes. Result is systemd's verdict on the target's scope (oom-kill means the cap killed it; n/a without systemd). Peak memory is the scope's MemoryPeak, sampled once a second while the target ran: parent and all workers together, page cache included; unknown without a systemd user scope or when systemd does not report it."
  echo
  echo "| target | seconds | executions | exec/s | cov | ft | corpus before | corpus after | crashes | timeouts | OOMs | artifacts | exit | result | peak memory |"
  echo "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"
  awk -F'\t' '{ printf "| %s | %s | %s | %s | %s | %s | %s / %s | %s / %s | %s | %s | %s | %s | %s | %s | %s |\n",
      $1, $2, $3, $6, $4, $5, $10, $11, $12, $13, $9, $8, $7, $14, $15, $16,
      ($17 ~ /^[0-9]+$/ ? sprintf("%d MiB", $17 / 1048576) : "unknown") }' "$runs"
} > "$pkg/report.md"

name=rift-fuzz-$label
archive=$outdir/$name.tar.gz
[ ! -e "$archive" ] || archive=$outdir/$name-$$.tar.gz
tar -C "$pkg" -czf "$archive" report.md results.json artifacts logs
echo "Done: $archive" >&2
