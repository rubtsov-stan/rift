#!/bin/sh
# Builds the site into site/public.
#
#   site/scripts/build.sh [zola build arguments]
#
# Fetches the GitHub numbers (keeps the last fetch if gh is unavailable),
# computes the benchmark data, renders the man page, then runs Zola.
# ZOLA names the binary, default zola.
set -eu

cd "$(dirname "$0")/.."
if ! scripts/fetch-stats.sh; then
  [ -f data/stats.json ] || { echo "build.sh: no data/stats.json and the fetch failed" >&2; exit 1; }
  echo "build.sh: fetch failed, using the numbers from the last fetch" >&2
fi
"${PYTHON:-python3}" scripts/bench-data.py
"${PYTHON:-python3}" scripts/render-man.py

# The media below goes where Zola writes, --output-dir or -o if given.
out=public prev=
for a in "$@"; do
  case $prev in -o | --output-dir) out=$a ;; esac
  case $a in --output-dir=*) out=${a#*=} ;; esac
  prev=$a
done

"${ZOLA:-zola}" build "$@"

# The extension demo and screenshots have one committed copy, in
# editors/vscode/media.
media=../editors/vscode/media
for f in mp4 gif; do
  [ -f "$media/rift-demo.$f" ] && cp "$media/rift-demo.$f" "$out/vscode-demo.$f"
done
if [ -d "$media/screens" ]; then
  mkdir -p "$out/vscode-screens"
  cp "$media"/screens/*.png "$out/vscode-screens/"
fi
exit 0
