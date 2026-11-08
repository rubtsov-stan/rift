#!/bin/sh
# Writes, next to every release archive in DIR, the files that let a reader
# check where it came from without calling the GitHub API:
#   <archive>.sigstore.json  the Sigstore bundle of its build attestation
#   <archive>.intoto.jsonl   the signed SLSA provenance, as DSSE envelopes
#                            (one per line) taken from that bundle
# The attestations are the ones actions/attest created in the build jobs;
# nothing is generated here. Each bundle is verified before it is kept.
#
# usage: provenance-assets.sh DIR [OWNER/REPO]   (needs gh, jq, a GH_TOKEN)
set -eu

dir=${1:?usage: provenance-assets.sh DIR [OWNER/REPO]}
repo=${2:-${GITHUB_REPOSITORY:?no repository given}}
dir=$(cd "$dir" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

found=0
for archive in "$dir"/*.tar.xz; do
  [ -e "$archive" ] || break
  found=1
  rm -f "$work"/*.jsonl
  # The attestation can lag the upload by a few seconds.
  n=0
  until (cd "$work" && gh attestation download "$archive" --repo "$repo" >/dev/null 2>&1); do
    n=$((n + 1))
    [ "$n" -lt 5 ] || { echo "no attestation for $archive" >&2; exit 1; }
    sleep 5
  done
  # One bundle per line; the first is enough, all describe the same build.
  head -n 1 "$work"/*.jsonl > "$archive.sigstore.json"
  jq -c '.dsseEnvelope' "$archive.sigstore.json" > "$archive.intoto.jsonl"
  gh attestation verify "$archive" --bundle "$archive.sigstore.json" --repo "$repo" >/dev/null
  # A SLSA provenance statement about exactly this file.
  digest=$(sha256sum "$archive" 2>/dev/null || shasum -a 256 "$archive")
  digest=${digest%% *}
  jq -r '.payload' "$archive.intoto.jsonl" | base64 -d | jq -e --arg d "$digest" \
    '.predicateType == "https://slsa.dev/provenance/v1" and any(.subject[]; .digest.sha256 == $d)' >/dev/null
  echo "ok ${archive##*/}"
done
[ "$found" = 1 ] || { echo "no *.tar.xz in $dir" >&2; exit 1; }
