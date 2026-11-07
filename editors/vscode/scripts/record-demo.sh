#!/bin/sh
# Records the extension demo: a Playwright-driven VS Code at 1280x800 comparing
# a generated pair, converted to MP4, GIF and a poster PNG.
#
#   scripts/record-demo.sh                 real run: rift-gen pair (1 GiB, seed 7), needs `rift serve`
#   scripts/record-demo.sh --mock          the mock server and two tiny files instead
#   --out DIR      where the media go (default editors/vscode/media)
#   --rift PATH    the rift binary (default $RIFT_BIN, else target/release/rift)
#   --size SIZE    rift-gen size (default 1G)
#
# Needs node, ffmpeg on PATH, and for a real run target/release/{rift,rift-gen}.
set -eu
cd "$(dirname "$0")/.."
command -v ffmpeg >/dev/null || { echo "ffmpeg is not on PATH" >&2; exit 1; }
npm run --silent build
exec node scripts/record-demo.mjs "$@"
