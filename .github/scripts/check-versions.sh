#!/bin/sh
# Fails when a file the release commit edits by hand or by tool carries a
# version other than [workspace.package] in Cargo.toml. See RELEASING.md step 1.
set -eu
cd "$(dirname "$0")/../.."

v=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
[ -n "$v" ] || { echo "check-versions: no version in Cargo.toml [workspace.package]" >&2; exit 1; }
bad=0
fail() { echo "check-versions: $1 says $2, Cargo.toml says $v" >&2; bad=1; }

# Packages built from this repository have no source line; rift-fuzz keeps
# its own 0.0.0.
for f in Cargo.lock fuzz/Cargo.lock; do
  out=$(awk '/^\[\[package\]\]/ { if (n != "" && !s) print n, ver; n = ""; ver = ""; s = 0 }
       /^name = / { n = $3 } /^version = / { ver = $3 } /^source = / { s = 1 }
       END { if (n != "" && !s) print n, ver }' "$f" | tr -d '"' |
    awk -v v="$v" '$1 != "rift-fuzz" && $2 != v')
  [ -z "$out" ] || { printf '%s\n' "$out" | sed "s|^|check-versions: $f: |;s|\$|, Cargo.toml says $v|" >&2; bad=1; }
done

p=$(jq -r .version editors/vscode/package.json)
[ "$p" = "$v" ] || fail editors/vscode/package.json "$p"
for q in .version '.packages[""].version'; do
  p=$(jq -r "$q" editors/vscode/package-lock.json)
  [ "$p" = "$v" ] || fail "editors/vscode/package-lock.json ($q)" "$p"
done
p=$(sed -n "s/.*client: { name: 'rift-vscode', version: '\([^']*\)' }.*/\1/p" editors/vscode/src/client.ts)
[ "$p" = "$v" ] || fail editors/vscode/src/client.ts "${p:-nothing}"
p=$(sed -n 's/^\.TH rift 1 *"rift \([^"]*\)".*/\1/p' man/rift.1)
[ "$p" = "$v" ] || fail man/rift.1 "${p:-nothing}"

[ "$bad" = 0 ] && echo "check-versions: $v everywhere"
exit "$bad"
