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
#   DIR/real/*.b.*                  each real file with 0.1% mutated lines
set -eu

dir=${1:-"$HOME/rift-data"}
root=$(cd "$(dirname "$0")/.." && pwd)
gen="$root/target/release/rift-gen"
spike="$root/target/release/examples/spike"

cargo build --release --manifest-path "$root/Cargo.toml" -p rift-gen
cargo build --release --manifest-path "$root/Cargo.toml" -p rift-core --example spike

mkdir -p "$dir/real"

[ -f "$dir/gen1g/manifest.tsv" ] || "$gen" --seed 7 --size 1G --profile sql --out "$dir/gen1g"
[ -f "$dir/gen10g/manifest.tsv" ] || "$gen" --seed 7 --size 10G --profile sql --out "$dir/gen10g"

cd "$dir/real"
if [ ! -f pagila.sql ]; then
  curl -sSL -o pagila-data.sql https://raw.githubusercontent.com/devrimgunduz/pagila/master/pagila-data.sql
  curl -sSL -o pagila-insert-data.sql https://raw.githubusercontent.com/devrimgunduz/pagila/master/pagila-insert-data.sql
  cat pagila-data.sql pagila-insert-data.sql > pagila.sql
  rm pagila-data.sql pagila-insert-data.sql
fi
if [ ! -f gh.ndjson ]; then
  for h in 10 11 12 13; do
    curl -sSL -o "gh-$h.json.gz" "https://data.gharchive.org/2024-01-15-$h.json.gz"
  done
  gunzip -c gh-10.json.gz gh-11.json.gz gh-12.json.gz gh-13.json.gz > gh.ndjson
  rm gh-*.json.gz
fi
if [ ! -f tlc.csv ]; then
  if python3 -c 'import pyarrow' 2>/dev/null; then
    curl -sSL -o tlc.parquet https://d37ci6vzurychx.cloudfront.net/trip-data/yellow_tripdata_2015-01.parquet
    python3 -c 'import pyarrow.parquet as pq, pyarrow.csv as pc; pc.write_csv(pq.read_table("tlc.parquet"), "tlc.csv")'
    rm tlc.parquet
  else
    echo "tlc.csv skipped: python3 with pyarrow is needed to convert the parquet file" >&2
  fi
fi
for f in pagila.sql gh.ndjson tlc.csv; do
  [ -f "$f" ] || continue
  b="${f%.*}.b.${f##*.}"
  [ -f "$b" ] || "$spike" mutate "$f" "$b" 1 0.001
done
echo "data ready in $dir"
