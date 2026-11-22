#!/bin/sh
# Checks the --cold logic of linux-safe.sh against a fake cpufreq tree and a
# fake sudo, on any host:
#
#   bench/cold-test.sh
#
# Covers the governor and energy_performance_preference save, set and restore
# (normal exit, error exit, SIGINT, SIGTERM, SIGHUP, a signal during the
# restore itself), the sudo keep-alive and its cleanup, the credential
# outliving its timeout, drops only before on-disk runs and the report text
# about them, the checks at the start (sudo for sh, writable drop_caches and
# governors), a host without cpufreq, an unreadable CPU, a failed restore
# being reported, and --dry-run touching nothing.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
script=$root/bench/linux-safe.sh
t=$(mktemp -d "${TMPDIR:-/tmp}/rift-cold-test.XXXXXX")
trap 'rm -rf "${t:?}"' EXIT

fails=0
check() { if [ "$1" = "$2" ]; then echo "ok: $3"; else echo "FAIL: $3: got '$1', wanted '$2'" >&2; fails=1; fi; }

# A sudo that accepts -n and runs the rest, one with no credential, and one
# that records being called.
# shellcheck disable=SC2016
printf '#!/bin/sh\necho "$*" >> "%s/sudo.log"\n[ "$1" = -n ] || exit 9\nshift\n[ "$1" = -v ] && exit 0\nexec "$@"\n' "$t" > "$t/sudo-ok"
printf '#!/bin/sh\nexit 1\n' > "$t/sudo-none"
printf '#!/bin/sh\ntouch "%s/sudo-called"\nexit 1\n' "$t" > "$t/sudo-mark"
# Allows `true` but not `sh`, like a sudoers with a command list.
# shellcheck disable=SC2016
printf '#!/bin/sh\n[ "$1" = -n ] || exit 9\n[ "$2" = true ] && exit 0\nexit 1\n' > "$t/sudo-notsh"
# A credential that lasts 2 s and is renewed by -v only while still valid.
# shellcheck disable=SC2016
printf '#!/bin/sh\n[ "$1" = -n ] || exit 9\nshift\nnow=$(date +%%s)\n[ "$now" -le "$(cat "%s/stamp")" ] || exit 1\n[ "$1" = -v ] && { echo $((now + 2)) > "%s/stamp"; exit 0; }\nexec "$@"\n' "$t" "$t" > "$t/sudo-timed"
# Sleeps 0.5 s before each command while $t/slow exists.
# shellcheck disable=SC2016
printf '#!/bin/sh\n[ "$1" = -n ] || exit 9\nshift\n[ -e "%s/slow" ] && sleep 0.5\nexec "$@"\n' "$t" > "$t/sudo-slow"
chmod +x "$t/sudo-slow" "$t/sudo-ok" "$t/sudo-none" "$t/sudo-mark" "$t/sudo-notsh" "$t/sudo-timed"

# tree DIR GOV...: one fake CPU per governor.
tree() {
  d=$1; shift; i=0
  for g in "$@"; do
    mkdir -p "$d/cpu$i/cpufreq"
    echo "$g" > "$d/cpu$i/cpufreq/scaling_governor"
    i=$((i + 1))
  done
}
govs() { cat "$1"/cpu*/cpufreq/scaling_governor | tr '\n' ' '; }

# prog BODY: the marked region of linux-safe.sh, the stubs it expects from
# the script, then BODY.
prog() {
  {
    echo 'die() { echo "linux-safe: $*" >&2; exit 1; }'
    sed -n '/^# cold:begin/,/^# cold:end/p' "$script"
    echo "$1"
  } > "$t/prog.sh"
}
export RIFT_SUDO="$t/sudo-ok" RIFT_DROP_CACHES="$t/drop"

tree "$t/s1" powersave schedutil
export RIFT_SYSFS_CPU="$t/s1"
# shellcheck disable=SC2016
prog 'cold_set; cat "$RIFT_SYSFS_CPU"/cpu*/cpufreq/scaling_governor | tr "\n" " "; cold_restore'
check "$(sh "$t/prog.sh")" "performance performance " "cold_set sets every CPU to performance"
check "$(govs "$t/s1")" "powersave schedutil " "cold_restore puts each CPU's own governor back"

# Restore on error and on signals, with the trap lines the script uses.
# shellcheck disable=SC2016
prog 'trap cold_restore EXIT; trap "exit 130" INT; trap "exit 143" TERM; cold_set; cat "$RIFT_SYSFS_CPU"/cpu*/cpufreq/scaling_governor > "$RIFT_DROP_CACHES.mid"; no_such_command_here'
rc=0; sh -e "$t/prog.sh" 2>/dev/null || rc=$?
check "$rc:$(govs "$t/s1"):$(tr '\n' ' ' < "$t/drop.mid")" "127:powersave schedutil :performance performance " \
  "governors set, then restored after an error exit"
prog 'trap cold_restore EXIT; trap "exit 130" INT; cold_set; kill -INT $$; sleep 5'
rc=0; sh -e "$t/prog.sh" 2>/dev/null || rc=$?
check "$rc:$(govs "$t/s1")" "130:powersave schedutil " "governors restored after SIGINT"
prog 'trap cold_restore EXIT; trap "exit 143" TERM; cold_set; kill -TERM $$; sleep 5'
rc=0; sh -e "$t/prog.sh" 2>/dev/null || rc=$?
check "$rc:$(govs "$t/s1")" "143:powersave schedutil " "governors restored after SIGTERM"

# A governor file that cannot be written at restore is reported, with the
# governor the CPU was left on. Root ignores file modes, so skip as root.
if [ "$(id -u)" -ne 0 ]; then
  tree "$t/s3" powersave schedutil
  # shellcheck disable=SC2016
  prog 'cold_set; chmod 444 "$RIFT_SYSFS_CPU/cpu1/cpufreq/scaling_governor"; cold_restore 2>/dev/null; echo "$cold_failed"'
  out=$(RIFT_SYSFS_CPU=$t/s3 sh "$t/prog.sh")
  check "$out|$(govs "$t/s3")" "cpu1 scaling_governor left on performance (wanted schedutil); |powersave performance " \
    "a failed restore is recorded with the governor left behind"
fi

# keepalive CMD: runs CMD after starting the keep-alive loop (interval 0.2 s)
# and records its pid in $t/keep.pid; then, whatever CMD did, checks that the
# loop called -v several times and that no process or call is left afterwards.
# $1 name, $2 expected exit code, $3 body run after the loop starts.
keep() {
  : > "$t/sudo.log"; rm -f "$t/keep.pid"
  # shellcheck disable=SC2016
  prog "trap 'cold_keep_stop' EXIT; trap 'exit 130' INT; trap 'exit 143' TERM; trap 'exit 129' HUP; cold_keepalive; echo \$cold_keep_pid > \"$t/keep.pid\"; sleep 1; $3"
  rc=0; RIFT_SUDO_KEEPALIVE=0.2 sh "$t/prog.sh" 2>/dev/null || rc=$?
  n1=$(grep -c '^-n -v$' "$t/sudo.log" || true)
  sleep 1
  n2=$(grep -c '^-n -v$' "$t/sudo.log" || true)
  alive=gone; kill -0 "$(cat "$t/keep.pid" 2>/dev/null || echo 0)" 2>/dev/null && alive=alive
  more=several; [ "$n1" -ge 2 ] || more=few
  stable=stopped; [ "$n1" = "$n2" ] || stable=running
  check "$rc:$more:$stable:$alive" "$2:several:stopped:gone" "keep-alive: $1"
}
keep "normal exit" 0 'true'
keep "error exit" 127 'no_such_command_here'
keep "SIGINT" 130 'kill -INT $$; sleep 5'
keep "SIGTERM" 143 'kill -TERM $$; sleep 5'
keep "SIGHUP" 129 'kill -HUP $$; sleep 5'

# SIGHUP (an SSH drop) restores the governors too, and the script arms it.
# shellcheck disable=SC2016
prog 'trap cold_restore EXIT; trap "exit 129" HUP; cold_set; kill -HUP $$; sleep 5'
rc=0; sh -e "$t/prog.sh" 2>/dev/null || rc=$?
check "$rc:$(govs "$t/s1")" "129:powersave schedutil " "governors restored after SIGHUP"
check "$(grep -c "^trap 'exit 129' HUP" "$script")" 1 "linux-safe.sh traps HUP"

# After a run longer than the credential lasts, restore still works while the
# keep-alive refreshes it; without the keep-alive it fails (control).
timed() { # timed WITH_KEEPALIVE
  tree "$t/s6" powersave schedutil
  echo $(($(date +%s) + 2)) > "$t/stamp"
  # shellcheck disable=SC2016
  prog "trap 'cold_keep_stop' EXIT; $1; cold_set; sleep 4; cold_restore 2>/dev/null; echo \"\$cold_failed\""
  RIFT_SUDO=$t/sudo-timed RIFT_SYSFS_CPU=$t/s6 RIFT_SUDO_KEEPALIVE=0.5 sh "$t/prog.sh" > "$t/timed.out" 2>/dev/null || true
  echo "$(govs "$t/s6")|$(tr -d '\n' < "$t/timed.out" | wc -c | tr -d ' ')"
}
check "$(timed cold_keepalive)" "powersave schedutil |0" "restore works after the credential's lifetime with the keep-alive"
check "$(timed true | cut -d'|' -f1)" "performance performance " "without the keep-alive the same run cannot restore (control)"

# A signal in the middle of the report-time restore: the exit trap finishes
# the CPUs that are left (each write takes 0.5 s, the signal comes at 0.7 s).
tree "$t/s10" powersave schedutil ondemand
: > "$t/slow"
# shellcheck disable=SC2016
prog 'trap cold_restore EXIT; trap "exit 130" INT; cold_set; ( sleep 0.7; kill -INT $$ ) & cold_restore; wait'
rc=0; RIFT_SUDO=$t/sudo-slow RIFT_SYSFS_CPU=$t/s10 sh "$t/prog.sh" 2>/dev/null || rc=$?
rm -f "$t/slow"
check "$rc:$(govs "$t/s10")" "130:powersave schedutil ondemand " "a signal during restore: the exit trap restores every CPU"

# What the report says about drops is what happened.
# shellcheck disable=SC2016
prog 'cold=1; cold_cache_desc; cold_run_drop disk; cold_run_drop read-only-pair; cold_run_drop tools; cold_cache_desc'
out=$(sh "$t/prog.sh" 2>/dev/null) || true
case $out in
  *"no on-disk group ran, no drop"*"dropped 2 times"*) echo "ok: cold_cache reports the drops that happened" ;;
  *) echo "FAIL: cold_cache text: $out" >&2; fails=1 ;;
esac
# shellcheck disable=SC2016
prog 'for v in "x||standard" "x||tools" "|y|standard" "||standard" "x|y|tools"; do diskdir=${v%%|*} r=${v#*|} ropair=${r%%|*} suite=${r#*|}; if cold_has_disk_group; then printf "yes "; else printf "no "; fi; done'
check "$(sh "$t/prog.sh" 2>/dev/null || true)" "yes no yes no yes " "cold_has_disk_group: --disk-dir needs a suite other than tools, --read-only-pair always counts"

# The RAM caveat stays with --cold when the pair fits in RAM.
# shellcheck disable=SC2016
prog 'cold=1; cold_ram_clause 2147483648 8589934592; cold_ram_clause 20000000000 8589934592'
out=$(sh "$t/prog.sh" 2>/dev/null) || true
case $out in
  *"not fully cold"*"exceeds RAM"*) echo "ok: the fits-in-RAM caveat is kept under --cold" ;;
  *) echo "FAIL: ram clause: $out" >&2; fails=1 ;;
esac

# Drops happen only before on-disk runs.
: > "$t/sudo.log"
# shellcheck disable=SC2016
prog 'cold=1; for g in all compare generated scaling le tools; do cold_run_drop "$g"; done; echo "tmpfs groups: $(grep -c "echo 3" "$RIFT_SUDO_LOG")"; for g in disk read-only-pair; do cold_run_drop "$g"; done; echo "after disk groups: $(grep -c "echo 3" "$RIFT_SUDO_LOG")"; cold=0; cold_run_drop disk; echo "cold off: $(grep -c "echo 3" "$RIFT_SUDO_LOG")"'
check "$(RIFT_SUDO_LOG=$t/sudo.log sh "$t/prog.sh" | tr '\n' ';')" "tmpfs groups: 0;after disk groups: 2;cold off: 2;" "cache dropped only before the disk groups"

# The host governor fact is read where the runs see it: after cold_set.
tree "$t/s7" powersave schedutil
# shellcheck disable=SC2016
prog 'a=$(cold_host_governor); cold_set; b=$(cold_host_governor); cold_restore; echo "$a $b"'
check "$(RIFT_SYSFS_CPU=$t/s7 sh "$t/prog.sh")" "powersave performance" "cold_host_governor reads the fake tree before and after cold_set"

# energy_performance_preference is saved and restored with the governor, and a
# failed restore of it is reported.
tree "$t/s8" powersave
echo balance_performance > "$t/s8/cpu0/cpufreq/energy_performance_preference"
# shellcheck disable=SC2016
prog 'cold_set; echo performance > "$RIFT_SYSFS_CPU/cpu0/cpufreq/energy_performance_preference"; cold_restore; echo "$(cat "$RIFT_SYSFS_CPU/cpu0/cpufreq/scaling_governor") $(cat "$RIFT_SYSFS_CPU/cpu0/cpufreq/energy_performance_preference")"'
check "$(RIFT_SYSFS_CPU=$t/s8 sh "$t/prog.sh")" "powersave balance_performance" "the energy_performance_preference is restored too"
if [ "$(id -u)" -ne 0 ]; then
  # shellcheck disable=SC2016
  prog 'cold_set; f=$RIFT_SYSFS_CPU/cpu0/cpufreq/energy_performance_preference; echo performance > "$f"; chmod 444 "$f"; cold_restore 2>/dev/null; echo "$cold_failed"'
  check "$(RIFT_SYSFS_CPU=$t/s8 sh "$t/prog.sh")" "cpu0 energy_performance_preference left on performance (wanted balance_performance); " \
    "a failed preference restore is reported"
fi

# A CPU whose governor cannot be read is skipped and reported, never saved as
# a malformed line.
if [ "$(id -u)" -ne 0 ]; then
  tree "$t/s9" powersave schedutil
  chmod 000 "$t/s9/cpu1/cpufreq/scaling_governor"
  # shellcheck disable=SC2016
  prog 'cold_set; echo "$cold_saved" | grep -c "^[a-z]* /"; echo "skipped: $cold_skipped"'
  check "$(RIFT_SYSFS_CPU=$t/s9 sh "$t/prog.sh" | tr '\n' ';')" "1;skipped: cpu1 ;" "an unreadable CPU is skipped and reported"
  chmod 644 "$t/s9/cpu1/cpufreq/scaling_governor"
fi

# The cache drop writes 3 to the file through sudo -n.
prog 'cold_drop'
sh "$t/prog.sh"
check "$(cat "$t/drop")" 3 "cold_drop writes 3 to drop_caches"

# No cpufreq: set and restore do nothing and succeed.
mkdir -p "$t/s2"
# shellcheck disable=SC2016
prog 'cold_set; echo "[$cold_saved]"; cold_restore'
check "$(RIFT_SYSFS_CPU=$t/s2 sh "$t/prog.sh")" "[]" "no cpufreq files: nothing saved, no error"

# A sudoers that allows `true` but not `sh` fails at the start too.
mkdir -p "$t/work"
rc=0
err=$(cd "$t/work" && RIFT_SUDO=$t/sudo-notsh RIFT_BENCH_TESTING=1 "$script" --cold --yes 2>&1) || rc=$?
case $err in
  *"sudo -v"*) check "$rc" 1 "a sudo that refuses sh stops --cold at the start" ;;
  *) echo "FAIL: sudo that refuses sh did not stop --cold at the start (rc $rc)" >&2; fails=1 ;;
esac

# The writes are proven at the start: an unwritable drop_caches or governor
# file stops the run there, with the governors as they were.
if [ "$(id -u)" -ne 0 ]; then
  tree "$t/s11" powersave schedutil
  rc=0
  err=$(cd "$t/work" && RIFT_SUDO=$t/sudo-ok RIFT_SYSFS_CPU=$t/s11 RIFT_DROP_CACHES=$t/nodir/drop RIFT_BENCH_TESTING=1 "$script" --cold --yes 2>&1) || rc=$?
  case $err in
    *"drop_caches"*|*"drop"*) check "$rc:$(govs "$t/s11")" "1:powersave schedutil " "an unwritable drop_caches stops --cold at the start" ;;
    *) echo "FAIL: unwritable drop_caches did not stop the run at the start (rc $rc): $err" >&2; fails=1 ;;
  esac
  chmod 444 "$t/s11/cpu1/cpufreq/scaling_governor"
  rc=0
  err=$(cd "$t/work" && RIFT_SUDO=$t/sudo-ok RIFT_SYSFS_CPU=$t/s11 RIFT_DROP_CACHES=$t/drop11 RIFT_BENCH_TESTING=1 "$script" --cold --yes 2>&1) || rc=$?
  case $err in
    *"scaling_governor"*) check "$rc:$(govs "$t/s11")" "1:powersave schedutil " "an unwritable governor file stops --cold at the start" ;;
    *) echo "FAIL: unwritable governor did not stop the run at the start (rc $rc): $err" >&2; fails=1 ;;
  esac
fi

# A missing credential stops the run before any governor or cache change.
rm -f "$t/drop"
rc=0
err=$(cd "$t/work" && RIFT_SUDO=$t/sudo-none RIFT_BENCH_TESTING=1 "$script" --cold --yes 2>&1) || rc=$?
check "$rc" 1 "--cold without a sudo credential exits 1"
case $err in
  *"sudo -v"*) echo "ok: the message says to run sudo -v" ;;
  *) echo "FAIL: no sudo -v hint: $err" >&2; fails=1 ;;
esac
dropped=untouched; [ ! -e "$t/drop" ] || dropped=dropped
check "$(govs "$t/s1")|$dropped|$(find "$t/work" -type f | wc -l | tr -d ' ')" "powersave schedutil |untouched|0" \
  "nothing changed after the sudo failure"

# --dry-run prints the plan and never calls sudo.
printf 'MemTotal: 33554432 kB\nMemAvailable: 33554432 kB\n' > "$t/meminfo"
mkdir "$t/shm"
rc=0
out=$(cd "$t/work" && RIFT_SUDO=$t/sudo-mark RIFT_BENCH_TESTING=1 RIFT_BENCH_MEMINFO=$t/meminfo RIFT_BENCH_SHM=$t/shm \
  "$script" --cold --dry-run 2>&1) || rc=$?
check "$rc" 0 "--cold --dry-run exits 0"
case $out in
  *"page cache"*"performance"*) echo "ok: the plan describes the cold runs" ;;
  *) echo "FAIL: plan lacks the cold line: $out" >&2; fails=1 ;;
esac
case $out in
  *"no on-disk group"*) echo "ok: the plan warns that --cold has no on-disk group to drop for" ;;
  *) echo "FAIL: no warning for --cold without an on-disk group: $out" >&2; fails=1 ;;
esac
mkdir "$t/ropair"; : > "$t/ropair/a"; : > "$t/ropair/b"
out=$(cd "$t/work" && RIFT_SUDO=$t/sudo-mark RIFT_BENCH_TESTING=1 RIFT_BENCH_MEMINFO=$t/meminfo RIFT_BENCH_SHM=$t/shm \
  "$script" --cold --dry-run --read-only-pair "$t/ropair" 2>&1) || true
case $out in
  *"no on-disk group"*) echo "FAIL: warning with a --read-only-pair: $out" >&2; fails=1 ;;
  *) echo "ok: no warning when --read-only-pair gives an on-disk group" ;;
esac
called=not-called; [ ! -e "$t/sudo-called" ] || called=called
check "$called|$(govs "$t/s1")" "not-called|powersave schedutil " "dry run calls no sudo and changes no governor"

# --suite none, --disk-cmds, --bin and ab-disk.sh, against a fake time, fake
# rift binaries and the fake sudo and cpufreq tree.
ab=$root/bench/ab-disk.sh
# GNU time -v writes its report to the -o file.
cat > "$t/faketime" <<'SH'
#!/bin/sh
shift
f=$2; shift 2
"$@"
printf '\tExit status: %s\n\tElapsed (wall clock) time (h:mm:ss or m:ss): 0:01.50\n\tUser time (seconds): 0.1\n\tSystem time (seconds): 0.1\n\tMaximum resident set size (kbytes): 100\n' "$?" > "$f"
SH
chmod +x "$t/faketime"
# sudo -v with and without -n, and commands.
# shellcheck disable=SC2016
printf '#!/bin/sh\n[ "$1" = -n ] && shift\n[ "$1" = -v ] && exit 0\nexec "$@"\n' > "$t/sudo-any"
chmod +x "$t/sudo-any"
# fakebin DIR OUTPUT LABEL: a rift that logs LABEL and prints OUTPUT.
fakebin() {
  mkdir -p "$1"
  # shellcheck disable=SC2016
  printf '#!/bin/sh\n[ "$1" = --version ] && { echo "rift 9.9.9 test"; exit 0; }\necho "%s" >> "$RIFT_TEST_LOG"\necho "peak heap 1.0 MiB of 256 MiB (1048576 bytes)" >&2\necho "%s"\nexit 1\n' "$3" "$2" > "$1/rift"
  printf '#!/bin/sh\nexit 0\n' > "$1/rift-gen"
  chmod +x "$1/rift" "$1/rift-gen"
}
fakebin "$t/binA" same base
fakebin "$t/binB" same head
fakebin "$t/binC" other head
tree "$t/s20" powersave schedutil
mkdir -p "$t/rw" "$t/work2" "$t/work3" "$t/disk"
echo a > "$t/rw/a"; echo b > "$t/rw/b"
stub() {
  env RIFT_BENCH_TESTING=1 RIFT_BENCH_MEMINFO="$t/meminfo" RIFT_BENCH_SHM="$t/shm" RIFT_BENCH_TIME="$t/faketime" \
    RIFT_BENCH_LIMITER=none RIFT_SYSFS_CPU="$t/s20" RIFT_DROP_CACHES="$t/drop20" RIFT_SUDO="$t/sudo-any" \
    RIFT_SUDO_KEEPALIVE=1 RIFT_TEST_LOG="$t/rift.log" "$@"
}
report() { tar -xzOf "$(find "$1" -name 'rift-bench-*.tar.gz' | head -n 1)" report.md 2>/dev/null || true; }
entries() { find "$1" -mindepth 1 -maxdepth 1 | sed 's|.*/||' | sort | tr '\n' ' '; }
rows() { printf '%s\n' "$1" | grep -c -e "$2" || true; }

# --suite none --disk-cmds --bin: only the listed commands run on the read-only
# pair, no tmpfs group, and the report names the binaries.
: > "$t/rift.log"
rc=0; (cd "$t/work2" && stub "$script" --yes --cold --reps 2 --suite none --bin "$t/binA" --read-only-pair "$t/rw" \
  --disk-cmds '--stats --mem 256M,cat') > "$t/work2/console" 2>&1 || rc=$?
rep=$(report "$t/work2")
check "$rc:$(rows "$rep" '^| [0-9]* | ')" "0:4" "--suite none --disk-cmds runs 2 commands twice and no tmpfs group"
check "$(rows "$rep" '^| [0-9]* | read-only-pair | existing | [0-9.]* [A-Za-z]* | \(rift --stats --mem 256M\|cat a b | wc -c\) |')" 4 \
  "--disk-cmds runs the listed commands on the read-only pair"
check "$(rows "$rep" '^| [0-9]* | .*\(--summary\|--mem 64M\)')" 0 "--disk-cmds leaves out the commands it does not list"
check "$(wc -l < "$t/rift.log" | tr -d ' ')" 2 "the listed rift command ran once per repetition"
sum=$(sha256sum "$t/binA/rift" | cut -d' ' -f1)
case $rep in
  *"- prebuilt_rift: sha256 $sum; rift 9.9.9 test"*"- rift_ref: prebuilt (--bin)"*) echo "ok: --bin records the binary's sha256 and --version" ;;
  *) echo "FAIL: the report lacks the prebuilt binary's hash and version" >&2; fails=1 ;;
esac
case $rep in
  *"- cold_cache: dropped 4 times"*) echo "ok: --cold dropped before each run of the on-disk group" ;;
  *) echo "FAIL: cold_cache line: $(printf '%s\n' "$rep" | grep cold_cache)" >&2; fails=1 ;;
esac

# Without the new options the read-only pair gets today's five commands.
rm -f "$t/work2"/rift-bench-*.tar.gz
rc=0; (cd "$t/work2" && stub "$script" --yes --cold --reps 1 --suite none --bin "$t/binA" --read-only-pair "$t/rw") > "$t/work2/console" 2>&1 || rc=$?
check "$rc:$(report "$t/work2" | grep '^| [0-9]* | read-only-pair ' | sed 's/^| [0-9]* | read-only-pair | existing | [0-9.]* [A-Za-z]* | //; s/ | all | .*//' | tr '\n' ';')" \
  "0:cat a b | wc -c;rift --summary --mem 256M;rift --stats --mem 256M;rift --stats --mem 64M;rift --stats --mem 1G;" \
  "the default set on the read-only pair is unchanged"

# Refusals.
for v in "--suite none|nothing else would run" "--disk-cmds --stats|needs --read-only-pair" \
         "--bin $t/binA --ref main --read-only-pair $t/rw --suite none|exclude each other" \
         "--bin $t/nope --read-only-pair $t/rw --suite none|must hold executable" \
         "--read-only-pair $t/rw --disk-cmds summary|neither cat nor rift arguments"; do
  rc=0
  # shellcheck disable=SC2086
  err=$(cd "$t/work2" && stub "$script" --dry-run ${v%%|*} 2>&1) || rc=$?
  case $err in *"${v#*|}"*) check "$rc" 1 "refused: ${v#*|}" ;; *) echo "FAIL: ${v%%|*}: $err" >&2; fails=1 ;; esac
done

# ab-disk.sh: the plan alternates base and head, round by round.
RIFT_AB_SIZE_MIB=1 stub "$ab" --dry-run "$t/binA" "$t/binB" "$t/disk" 3 > "$t/ab.plan" 2>&1 || true
check "$(grep -E '^    [0-9]+\. ' "$t/ab.plan" | awk '{printf "%s%s ", $1, $2}')" "1.base 1.head 2.base 2.head 3.base 3.head " \
  "ab-disk.sh --dry-run alternates base and head"
check "$(grep -c -- "--cold --reps 1 --suite none .*--disk-cmds '--stats --mem 256M'" "$t/ab.plan")" 6 "ab-disk.sh plans the default command for every run"
check "$(entries "$t/disk")" "" "ab-disk.sh --dry-run creates nothing"

# ab-disk.sh end to end on stand-ins: the identity check, then base, head,
# base, head; --keep leaves the pair, the default removes it.
mkdir "$t/disk/rift-ab-pair"; echo a > "$t/disk/rift-ab-pair/a"; echo b > "$t/disk/rift-ab-pair/b"
: > "$t/rift.log"
rc=0; (cd "$t/work3" && RIFT_AB_SIZE_MIB=1 stub "$ab" --keep "$t/binA" "$t/binB" "$t/disk" 2) > "$t/ab.out" 2> "$t/ab.err" || rc=$?
check "$rc:$(tr '\n' ' ' < "$t/rift.log")" "0:base head base head base head " "ab-disk.sh: identity check, then base and head alternate"
check "$(grep -c '^base: median 1.5 s\|^head: median 1.5 s' "$t/ab.out"):$(grep -c '^[12]	\(base\|head\)	1.5	1	' "$t/ab.out")" "2:4" \
  "ab-disk.sh summary has both medians and the per-run rows"
check "$(find "$t/work3" -name 'rift-ab-disk-*.tar.gz' | wc -l | tr -d ' '):$(entries "$t/disk")" "1:rift-ab-pair " \
  "ab-disk.sh packs a tarball, removes its work directory and keeps the pair with --keep"
rc=0; (cd "$t/work3" && RIFT_AB_SIZE_MIB=1 stub "$ab" "$t/binA" "$t/binB" "$t/disk" 1) > /dev/null 2>&1 || rc=$?
check "$rc:$(entries "$t/disk")" "0:" "ab-disk.sh deletes the pair without --keep"
mkdir "$t/disk/rift-ab-pair"; echo a > "$t/disk/rift-ab-pair/a"; echo b > "$t/disk/rift-ab-pair/b"
: > "$t/rift.log"
rc=0; err=$(cd "$t/work3" && RIFT_AB_SIZE_MIB=1 stub "$ab" "$t/binA" "$t/binC" "$t/disk" 2 2>&1) || rc=$?
case $err in
  *"stdout differs"*) check "$rc:$(wc -l < "$t/rift.log" | tr -d ' ')" "1:2" "ab-disk.sh stops before timing when the outputs differ" ;;
  *) echo "FAIL: differing outputs did not stop ab-disk.sh: $err" >&2; fails=1 ;;
esac
# Two builds that print nothing are not a match, and an unknown BASE is refused
# even when HEAD resolves.
mkdir -p "$t/binE"
printf '#!/bin/sh\nexit 2\n' > "$t/binE/rift"; cp "$t/binE/rift" "$t/binE/rift-gen"; chmod +x "$t/binE/rift" "$t/binE/rift-gen"
mkdir "$t/disk/rift-ab-pair"; echo a > "$t/disk/rift-ab-pair/a"; echo b > "$t/disk/rift-ab-pair/b"
rc=0; err=$(cd "$t/work3" && RIFT_AB_SIZE_MIB=1 stub "$ab" "$t/binE" "$t/binE" "$t/disk" 1 2>&1) || rc=$?
case $err in
  *"printed nothing"*) check "$rc" 1 "ab-disk.sh does not take two empty outputs for a match" ;;
  *) echo "FAIL: empty outputs passed the identity check: $err" >&2; fails=1 ;;
esac
rc=0; (cd "$t/work3" && RIFT_AB_SIZE_MIB=1 stub "$ab" --dry-run no-such-ref-here "$t/binB" "$t/disk" 1) > /dev/null 2>&1 || rc=$?
check "$rc" 1 "ab-disk.sh refuses an unknown BASE when HEAD resolves"
exit "$fails"
