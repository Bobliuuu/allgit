#!/bin/sh
# Usage: run_lab.sh <name> <source-spec> [extra flags]   (run on the benchmark box, from the workspace root)
# Writes JSON to $LAB_OUT (default ~/rt/lab_out). Sequential on purpose so timings are not contaminated by each other.
set -e
NAME="$1"; SPEC="$2"; shift 2
L=${LAB_BIN:-crates/vcs-compress-lab/target/release/compress-lab}
O=${LAB_OUT:-$HOME/rt/lab_out}
mkdir -p "$O"
echo "== $NAME start $(date -u +%FT%TZ) load: $(cat /proc/loadavg)"
for exp in codecs chunking; do
  $L $exp --name "$NAME" --source "$SPEC" --out "$O/${NAME}_$exp.json" "$@"
done
$L blocks --name "$NAME" --source "$SPEC" --order path --tmp "${LAB_TMP:-/tmp}" --out "$O/${NAME}_blocks_path.json" "$@"
$L blocks --name "$NAME" --source "$SPEC" --order first-seen --tmp "${LAB_TMP:-/tmp}" --out "$O/${NAME}_blocks_firstseen.json" "$@"
echo "== $NAME end $(date -u +%FT%TZ) load: $(cat /proc/loadavg)"
