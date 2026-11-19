#!/bin/sh
# Checks compare.sh's output comparison with stand-in rift binaries that
# print a few lines and take no time:
#
#   bench/compare-test.sh
#
# A head that reorders base's lines fails by default and passes with
# SORTED=1; a head that changes a line, or prints its own runs in different
# orders, fails either way. Takes compare.sh's lock like any run of it.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
t=$(mktemp -d "${TMPDIR:-/tmp}/rift-compare-test.XXXXXX")
trap 'rm -rf "${t:?}"' EXIT

# fake NAME BODY: a stand-in rift that prints BODY's output and a peak heap line.
fake() {
  mkdir -p "$t/$1"
  printf '#!/bin/sh\n%s\necho "peak heap 1.0 KiB (1000 bytes)" >&2\n' "$2" > "$t/$1/rift"
  chmod +x "$t/$1/rift"
}
fake base 'printf "a\nb\nc\n"'
fake reordered 'printf "c\nb\na\n"'
fake changed 'printf "c\nb\nx\n"'
# Alternates its order from run to run.
fake flaky "f=$t/flaky/n; n=\$(cat \"\$f\" 2>/dev/null || echo 0); echo \$((n + 1)) > \"\$f\"
if [ \$((n % 2)) = 0 ]; then printf 'a\nb\nc\n'; else printf 'c\nb\na\n'; fi"
# rift-gen beside each head: writes empty files where compare.sh expects a pair.
for d in reordered changed flaky; do
  # shellcheck disable=SC2016
  printf '#!/bin/sh\nwhile [ "$1" != --out ]; do shift; done\nmkdir -p "$2"; : > "$2/a"; : > "$2/b"; : > "$2/manifest.tsv"\n' > "$t/$d/rift-gen"
  chmod +x "$t/$d/rift-gen"
done

fails=0
# expect RC SORTED HEAD: runs compare.sh on one tiny pair, checks its exit code.
expect() {
  rc=0
  SORTED=$2 RUNS=2 PAIRS='t|x|' "$root/bench/compare.sh" "$t/base/rift" "$t/$3/rift" "$t/scratch-$3-$2" \
    > "$t/out" 2> "$t/err" || rc=$?
  if [ "$rc" = "$1" ]; then echo "ok: SORTED=$2 $3 exits $rc"
  else echo "FAIL: SORTED=$2 $3 exits $rc, wanted $1" >&2; cat "$t/err" >&2; fails=1; fi
}
expect 1 0 reordered
expect 0 1 reordered
expect 1 1 changed
expect 1 0 changed
expect 1 1 flaky
exit "$fails"
