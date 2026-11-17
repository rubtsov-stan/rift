#!/bin/sh
# Builds the MCP bundle of rift-mcp for a release from the release's own
# rift-mcp archives, and the server.json the MCP Registry gets for it.
#
# usage: mcpb-bundle.sh TAG ARCHIVES OUT [OWNER/REPO]
#   ARCHIVES  holds rift-mcp-<target>.tar.xz for the four dist targets
#   OUT       gets rift-mcp-<version>.mcpb and server.json
# Needs lipo (macOS), jq and zip. The templates are in .github/mcpb/.
#
# One bundle for every platform: MCPB tells platforms apart by OS only, and
# server.json has no platform field, so a per-platform bundle could not be
# picked. The launcher chooses the binary by uname; macOS gets one universal
# binary.
set -eu

tag=${1:?usage: mcpb-bundle.sh TAG ARCHIVES OUT [OWNER/REPO]}
archives=$(cd "${2:?no archive directory}" && pwd)
out=${3:?no output directory}
repo=${4:-${GITHUB_REPOSITORY:-rubtsov-stan/rift}}
version=${tag#v}
here=$(cd "$(dirname "$0")/../mcpb" && pwd)
mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

for t in aarch64-apple-darwin x86_64-apple-darwin aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
  mkdir -p "$work/$t"
  tar -xJf "$archives/rift-mcp-$t.tar.xz" -C "$work/$t" --strip-components 1
done
b=$work/bundle
mkdir -p "$b/server"
lipo -create -output "$b/server/rift-mcp-darwin" "$work/aarch64-apple-darwin/rift-mcp" "$work/x86_64-apple-darwin/rift-mcp"
cp "$work/x86_64-unknown-linux-gnu/rift-mcp" "$b/server/rift-mcp-linux-x86_64"
cp "$work/aarch64-unknown-linux-gnu/rift-mcp" "$b/server/rift-mcp-linux-aarch64"
cp "$here/rift-mcp" "$b/server/rift-mcp"
chmod 755 "$b"/server/*
jq --arg v "$version" '.version = $v' "$here/manifest.json" > "$b/manifest.json"

# The bundle, server.json and the registry take the version from the tag; the
# binary reports its own, from Cargo.toml. A published registry version cannot
# be replaced, so a mismatch stops here. Starting it also shows it runs.
init='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"mcpb-bundle","version":"0"}}}'
reported=$(printf '%s\n' "$init" | "$b/server/rift-mcp" | head -n 1 | jq -r '.result.serverInfo.version')
[ "$reported" = "$version" ] || { echo "mcpb-bundle: rift-mcp reports version $reported, the tag says $version" >&2; exit 1; }

# The fields the MCPB manifest requires, and the binary server it describes.
jq -e '.manifest_version and .name and .version and .description and .author.name
  and .server.type == "binary" and .server.entry_point == "server/rift-mcp"' "$b/manifest.json" >/dev/null

# An MCPB bundle is a zip with manifest.json at its root. zip keeps each
# file's mode, so the launcher and the binaries stay executable; -X leaves
# out owner and timestamp extras.
name=rift-mcp-$version.mcpb
rm -f "$out/$name"
(cd "$b" && zip -X -q -9 -r "$out/$name" manifest.json server)
sha=$(shasum -a 256 "$out/$name")
sha=${sha%% *}
jq --arg v "$version" --arg url "https://github.com/$repo/releases/download/$tag/$name" --arg sha "$sha" \
  '.version = $v | .packages[0].identifier = $url | .packages[0].fileSha256 = $sha' \
  "$here/server.json" > "$out/server.json"
echo "$out/$name $sha"
