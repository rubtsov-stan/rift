#!/bin/sh
# Builds the site into site/public.
#
#   site/scripts/build.sh [zola build arguments]
#
# Fetches the GitHub numbers (keeps the last fetch if gh is unavailable),
# computes the benchmark data, renders the man page, then runs Zola.
# ZOLA names the binary, default zola. RIFT_SITE_VSCODE=1 includes the
# VS Code pages and section, which stay out of the build until the 0.3 release.
set -eu

cd "$(dirname "$0")/.."
if ! scripts/fetch-stats.sh; then
  [ -f data/stats.json ] || { echo "build.sh: no data/stats.json and the fetch failed" >&2; exit 1; }
  echo "build.sh: fetch failed, using the numbers from the last fetch" >&2
fi
"${PYTHON:-python3}" scripts/bench-data.py
"${PYTHON:-python3}" scripts/render-man.py

drafts=
[ "${RIFT_SITE_VSCODE:-0}" = 1 ] && drafts=--drafts
# shellcheck disable=SC2086
"${ZOLA:-zola}" build $drafts "$@"

# The extension demo has one committed copy, in editors/vscode/media. Only a
# build that links it (RIFT_SITE_VSCODE=1) gets it.
if [ "${RIFT_SITE_VSCODE:-0}" = 1 ]; then
  media=../editors/vscode/media
  for f in mp4 gif; do
    [ -f "$media/rift-demo.$f" ] && cp "$media/rift-demo.$f" "public/vscode-demo.$f"
  done
fi
exit 0
