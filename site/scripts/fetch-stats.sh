#!/bin/sh
# Writes the GitHub numbers the landing page bakes into its HTML.
#
#   site/scripts/fetch-stats.sh [OWNER/REPO] [OUT_FILE]
#
# Needs gh (authenticated; in CI GH_TOKEN) and curl. The nightly site build runs it.
# "release" is the tag of the latest published release, empty when there is none.
set -eu

repo=${1:-rubtsov-stan/rift}
out=${2:-"$(cd "$(dirname "$0")/.." && pwd)/data/stats.json"}

stars=$(gh api "repos/$repo" --jq .stargazers_count)
release=$(gh api "repos/$repo/releases/latest" --jq .tag_name 2>/dev/null || echo "")
downloads=$(gh api --paginate "repos/$repo/releases" --jq '.[].assets[].download_count' | awk '{s += $1} END {print s + 0}')
ci=$(gh api "repos/$repo/actions/workflows/ci.yml/runs?branch=main&per_page=1&status=completed" --jq '.workflow_runs[0].conclusion // "unknown"')
# The Scorecard API has no entry for a repository it has not scanned yet.
score=$(curl -fsS "https://api.securityscorecards.dev/projects/github.com/$repo" | grep -o '"score":[0-9.]*' | head -n 1 | cut -d: -f2 || true)

mkdir -p "$(dirname "$out")"
printf '{"repo":"%s","stars":%s,"release":"%s","downloads":%s,"ci":"%s","scorecard":%s,"fetched":"%s"}\n' \
  "$repo" "$stars" "$release" "$downloads" "$ci" "${score:-null}" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" >"$out"
