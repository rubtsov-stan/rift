#!/bin/sh
# Renders docs/logo.svg to the 128x128 extension icon. Uses sips, which ships
# with macOS; the committed media/icon.png was made this way.
set -eu
cd "$(dirname "$0")/.."
sips -s format png -z 128 128 ../../docs/logo.svg --out media/icon.png
