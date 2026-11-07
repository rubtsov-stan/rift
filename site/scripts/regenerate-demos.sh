#!/bin/sh
# Records the demo GIFs and MP4s with vhs and cuts a poster image from the first
# frame of each. Run from anywhere; needs vhs (with ttyd and ffmpeg).
#
#   site/scripts/regenerate-demos.sh
#
# Output goes to site/static: NAME.gif, NAME.mp4 and NAME-poster.jpg. Each GIF
# must stay under 2 MB; the script fails when one does not.
set -eu

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
cargo build --release -p rift-diff -p rift-gen

for tape in site/tapes/*.tape; do
  name=$(basename "$tape" .tape)
  vhs "$tape"
  # The first frame is an empty prompt. A tape names the second its poster comes from.
  secs=$(sed -n 's/^# poster: //p' "$tape")
  ffmpeg -y -loglevel error -ss "${secs:-3}" -i "site/static/$name.mp4" -frames:v 1 -q:v 4 "site/static/$name-poster.jpg"
  size=$(wc -c <"site/static/$name.gif")
  if [ "$size" -ge 2000000 ]; then
    echo "regenerate-demos: $name.gif is $size bytes, over the 2 MB limit" >&2
    exit 1
  fi
  echo "$name.gif $size bytes"
done
