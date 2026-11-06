#!/bin/sh
# Regenerates the benchmark and spike data sets outside the repository.
#
#   bench/data.sh [DIR]      default DIR is ~/rift-data
#
# Produces:
#   DIR/gen1g/{a,b,manifest.tsv}    rift-gen seed 7, sql, 1 GiB
#   DIR/gen10g/{a,b,manifest.tsv}   rift-gen seed 7, sql, 10 GiB
#   DIR/real/pagila.sql             Pagila pg_dump (data + insert files)
#   DIR/real/gh.ndjson              GH Archive 2024-01-15 hours 10 to 13
#   DIR/real/tlc.csv                NYC TLC yellow 2015-01 (needs python3 + pyarrow)
#   DIR/real/bgl.log                Loghub BGL, message part of each line (needs unzip)
#   DIR/real/*.b.*                  each real file with 0.1% mutated lines
set -eu

dir=${1:-"$HOME/rift-data"}
root=$(cd "$(dirname "$0")/.." && pwd)
gen="$root/target/release/rift-gen"
spike="$root/target/release/examples/spike"

cargo build --release --manifest-path "$root/Cargo.toml" -p rift-gen
cargo build --release --manifest-path "$root/Cargo.toml" -p rift-core --example spike

mkdir -p "$dir/real"

# Generates into a sibling temp directory and renames it when complete, so an
# interrupted run leaves no half-written pair that looks finished.
gen_pair() { # name size
  [ -f "$dir/$1/manifest.tsv" ] && return 0
  rm -rf "$dir/$1.part"
  "$gen" --seed 7 --size "$2" --profile sql --out "$dir/$1.part"
  rm -rf "${dir:?}/$1"
  mv "$dir/$1.part" "$dir/$1"
}
gen_pair gen1g 1G
gen_pair gen10g 10G

# curl -f turns an HTTP error into a failure instead of saving the error page.
fetch() { # url out
  curl -fsSL -o "$2.part" "$1" && mv "$2.part" "$2"
}

cd "$dir/real"
if [ ! -f pagila.sql ]; then
  fetch https://raw.githubusercontent.com/devrimgunduz/pagila/master/pagila-data.sql pagila-data.sql
  fetch https://raw.githubusercontent.com/devrimgunduz/pagila/master/pagila-insert-data.sql pagila-insert-data.sql
  cat pagila-data.sql pagila-insert-data.sql > pagila.sql.part
  mv pagila.sql.part pagila.sql
  rm pagila-data.sql pagila-insert-data.sql
fi
if [ ! -f gh.ndjson ]; then
  for h in 10 11 12 13; do
    fetch "https://data.gharchive.org/2024-01-15-$h.json.gz" "gh-$h.json.gz"
  done
  gunzip -c gh-10.json.gz gh-11.json.gz gh-12.json.gz gh-13.json.gz > gh.ndjson.part
  mv gh.ndjson.part gh.ndjson
  rm gh-*.json.gz
fi
if [ ! -f tlc.csv ]; then
  if python3 -c 'import pyarrow' 2>/dev/null; then
    fetch https://d37ci6vzurychx.cloudfront.net/trip-data/yellow_tripdata_2015-01.parquet tlc.parquet
    python3 -c 'import pyarrow.parquet as pq, pyarrow.csv as pc; pc.write_csv(pq.read_table("tlc.parquet"), "tlc.csv.part")'
    mv tlc.csv.part tlc.csv
    rm tlc.parquet
  else
    echo "tlc.csv skipped: python3 with pyarrow is needed to convert the parquet file" >&2
  fi
fi
# Loghub BGL: Zenodo record 8196385, CC BY 4.0. Every full line
# is different (microsecond timestamps), so only fields 7 on, from "RAS" to
# the end of the message, are kept: 359k different lines out of 4.7M, 299 MiB.
bgl_url='https://zenodo.org/records/8196385/files/BGL.zip?download=1'
bgl_zip_sha256=d67fd82a711aea0157a9b83175892c6ee60e384a2ddf5bc51f39118453816da8
bgl_sha256=a56a414f612d729db654ce36f7dfc1672fe1686a1727f5ccd9335f21bdfe0323
sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
if [ ! -f bgl.log ]; then
  if command -v unzip >/dev/null 2>&1; then
    fetch "$bgl_url" bgl.zip
    [ "$(sha256 bgl.zip)" = "$bgl_zip_sha256" ] || { echo "bgl.zip: sha256 mismatch" >&2; exit 1; }
    unzip -p bgl.zip BGL.log | LC_ALL=C cut -d' ' -f7- > bgl.log.part
    [ "$(sha256 bgl.log.part)" = "$bgl_sha256" ] || { echo "bgl.log: sha256 mismatch" >&2; exit 1; }
    mv bgl.log.part bgl.log
    rm bgl.zip
  else
    echo "bgl.log skipped: unzip is needed to unpack the Loghub archive" >&2
  fi
fi
for f in pagila.sql gh.ndjson tlc.csv bgl.log; do
  [ -f "$f" ] || continue
  b="${f%.*}.b.${f##*.}"
  [ -f "$b" ] || { "$spike" mutate "$f" "$b.part" 1 0.001 && mv "$b.part" "$b"; }
done
echo "data ready in $dir"
