#!/bin/sh
# Regenerates every piece of real Rift output the site shows.
#
#   site/scripts/capture-examples.sh [OUT_DIR]     default OUT_DIR is site/data/captures
#
# Data recipe, all rift-gen with seed 7 in a temporary directory:
#   sql      64 MiB pair, renamed a.sql and b.sql
#   csv      32 MiB pair, profile shuffled (same keys, moved blocks), a.csv and b.csv
#   ndjson   32 MiB pair, profile shuffled with ndjson rows, a.ndjson and b.ndjson
#   logs     64 MiB pair, profile ndjson
#   big      1 GiB sql pair for the memory example
#   huge     640 MiB sql pair, over the 512 MiB limit git uses to call a file binary
# Needs jq, for the rift-mcp example.
# Every example is a shell command. The command goes to NAME.cmd and what it
# printed, standard output and error together as a terminal shows them, to
# NAME.out. The page prints both, so the command shown is the command that ran.
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
out=${1:-"$root/site/data/captures"}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cargo build --release --manifest-path "$root/Cargo.toml" -p rift-gen -p rift-diff -p rift-mcp
gen="$root/target/release/rift-gen"
PATH="$root/target/release:$PATH"
mkdir -p "$out"

mk() { # dir size profile ext [rift-gen args]
  d=$1; s=$2; p=$3; e=$4; shift 4
  "$gen" --seed 7 --size "$s" --profile "$p" "$@" --out "$work/$d" >/dev/null
  mv "$work/$d/a" "$work/$d/a.$e"; mv "$work/$d/b" "$work/$d/b.$e"
}
cap() { # name command: run in the current directory
  printf '%s\n' "$2" >"$out/$1.cmd"
  sh -c "$2" >"$out/$1.out" 2>&1 || true
}

mk sql 64M sql sql
cd "$work/sql"
cap sql-summary 'rift --summary a.sql b.sql'
cap sql-stats 'rift --stats a.sql b.sql > changes.patch'
cap sql-no-moves 'rift --no-moves --summary a.sql b.sql | tail -n 6'
cap mem-error 'rift --summary --mem 512K a.sql b.sql'
limit=$(sed -n 's/.*rerun with --mem \([0-9A-Za-z]*\).*/\1/p' "$out/mem-error.out")
# The named limit can fail at a later stage and name a higher one.
cap mem-rerun "rift --summary --mem $limit a.sql b.sql"
cap mem-ok 'rift --summary --mem 2M a.sql b.sql | tail -n 2'
cap exit-codes 'rift a.sql a.sql > /dev/null; echo "identical: exit $?"
rift a.sql b.sql > /dev/null; echo "differ: exit $?"
rift a.sql missing.sql > /dev/null; echo "missing file: exit $?"'
mkdir "$work/patchtest" && cp a.sql b.sql "$work/patchtest"
( cd "$work/patchtest"
  cap sql-patch 'rift a.sql b.sql > changes.patch
patch a.sql changes.patch
cmp a.sql b.sql && echo identical' )
rift a.sql b.sql >"$work/full.patch" || [ $? -eq 1 ]
# The page shows the first and the third hunk, with the timestamps dropped and
# long lines cut at 70 columns. The landing page shows the first hunk only.
trim() {
  LC_ALL=C awk -v dots="$(printf '\342\200\246')" '/^(---|\+\+\+) / { split($0, p, "\t"); $0 = p[1] } length($0) > 70 { $0 = substr($0, 1, 69) dots } { print }'
}
{ head -n 10 "$work/full.patch"; sed -n 28,35p "$work/full.patch"; } | trim >"$out/sql-diff-excerpt.out"
echo 'rift a.sql b.sql' >"$out/sql-diff-excerpt.cmd"
head -n 10 "$work/full.patch" | trim >"$out/land-diff.out"
echo 'rift a.sql b.sql' >"$out/land-diff.cmd"
cap land-summary 'rift --summary a.sql b.sql | grep -E "^(a:|b:|equal|differs|peak heap|time)"'
grep -c '^@@' "$work/full.patch" >"$out/sql-hunks.txt"
# The lines the landing page shows, picked from the full summary.
awk '
  /^a:/ || /^b:/ || /^equal/ || /^differs/ || /^peak heap/ || /^time/ { print; next }
  /^first differing/ { print; r = 2; next }
  r > 0 { print; r--; next }
  /^moved/ { print; m = 1; next }
  m == 1 { print; m = 0 }
' "$out/sql-summary.out" >"$out/hero.out"
echo 'rift --summary a.sql b.sql' >"$out/hero.cmd"
cd "$work"

mk csv 32M shuffled csv --rows csv
cd "$work/csv"
cap key-csv 'rift --key csv:id a.csv b.csv > keys.txt
head -n 8 keys.txt
wc -l < keys.txt'
cap land-key 'rift --key csv:id a.csv b.csv > keys.txt
for k in "~" "-" "+"; do grep -m 2 "^$k" keys.txt; done
cut -c1 keys.txt | sort | uniq -c'
head -n 2 a.csv | cut -c1-100 >"$out/csv-head.out"
echo 'head -n 2 a.csv' >"$out/csv-head.cmd"
cd "$work"

mk ndjson 32M shuffled ndjson --rows ndjson
cd "$work/ndjson"
cap ndjson-sample 'head -n 2 a.ndjson | cut -c1-110'
cap key-ndjson 'rift --key json:id a.ndjson b.ndjson > keys.txt
head -n 8 keys.txt
wc -l < keys.txt'
cd "$work"

mk logs 64M ndjson ndjson
cd "$work/logs"
cap ndjson-summary 'rift --summary a.ndjson b.ndjson'
cd "$work"

mk big 1G sql sql
cd "$work/big"
cap big-summary 'rift --summary --mem 64M a.sql b.sql'
cd "$work"
rm -rf "$work/big"

mk huge 640M sql sql
cd "$work/huge"
cap git-binary 'git diff --no-index a.sql b.sql'
cap git-rift "git -c diff.external='rift git-diff' diff --no-index a.sql b.sql > out.diff
head -n 12 out.diff | cut -c1-100"
# Git as the caller: a scratch repository, the driver set in .gitattributes and
# the config. The shown command is the setup the reader types, then git diff.
mkdir "$work/repo" && cd "$work/repo"
git init -q
cp "$work/huge/a.sql" dump.sql
git add dump.sql
git -c user.name=example -c user.email=example@example.com commit -q -m 'add dump'
cp "$work/huge/b.sql" dump.sql
cap git-driver "echo '*.sql diff=rift' >> .gitattributes
git config diff.rift.command 'rift git-diff --mem 1G'
git diff dump.sql > out.diff
head -n 10 out.diff | cut -c1-90"
cd "$work"

# rift-mcp over standard input and output: the handshake, then one tools/call
# for summary, sent after a pause so the answer comes back before input closes.
# The page shows the call without its jsonrpc and id fields, and the answer
# without its text copy, the byte counts, and all but the first two ranges.
mkdir "$work/mcp" && cp "$work/sql/a.sql" "$work/sql/b.sql" "$work/mcp" && cd "$work/mcp"
call='{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"summary","arguments":{"a":"a.sql","b":"b.sql"}}}'
{
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"example-client","version":"1.0"}}}' '{"jsonrpc":"2.0","method":"notifications/initialized"}'
  sleep 1
  printf '%s\n' "$call"
  sleep 3
} | rift-mcp >answers.jsonl
{
  printf -- '-> %s\n' "$(printf '%s' "$call" | jq -c '{method, params}')"
  printf '<- '
  jq -r 'select(.id == 2) | .result.structuredContent
    | del(.a_bytes, .b_bytes, .approx, .identical, .mem_reserved_bytes)
    | to_entries
    | map(if .key == "first_differences"
        then "  \"first_differences\": [\n" + (.value[:2] | map("    " + tojson) | join(",\n")) + ",\n    ...\n  ]"
        else "  \"\(.key)\": \(.value | tojson)" end)
    | "{\n" + join(",\n") + "\n}"' answers.jsonl
} >"$out/mcp-summary.out"
echo 'rift-mcp' >"$out/mcp-summary.cmd"
cd "$work"

{
  rift --version
  echo "built from $(git -C "$root" rev-parse --short HEAD)"
  case $(uname -s) in
    Darwin) echo "$(sysctl -n machdep.cpu.brand_string), $(( $(sysctl -n hw.memsize) / 1000000000 )) GB RAM, macOS $(sw_vers -productVersion)" ;;
    *) uname -sr ;;
  esac
} >"$out/meta.txt"
