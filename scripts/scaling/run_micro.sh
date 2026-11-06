#!/usr/bin/env bash
# Per-component cost-vs-N sweeps (synthetic structures, real reader code). Output: JSON lines per
# component in $OUT/<component>.jsonl. Usage: run_micro.sh LANE   (LANE = idx | graph | other)
# Env: BIN (scalebench), WORK (scratch on NVMe), OUT. Files are deleted after each N to save disk.
set -u
BIN=${BIN:-target/release/scalebench}
WORK=${WORK:-/data/scaling-work}
OUT=${OUT:-docs/research/data/scaling}
mkdir -p "$WORK" "$OUT"
LANE=${1:?lane}
run() { # file, then args
  local f=$1; shift
  /usr/bin/time -f "%e s wall, %M KB maxrss: $*" "$BIN" "$@" >> "$OUT/$f.jsonl" 2>> "$OUT/$f.time" || echo "FAILED: $*" >> "$OUT/$f.time"
}
case $LANE in
idx)
  for n in 1000 10000 100000 1000000 10000000 100000000 1000000000; do
    run pack_index idx "$n" "$WORK/idx$n" --lookups 20000 $([ "$n" -le 100000000 ] && echo "--verify 1")
    [ "$n" -lt 1000000000 ] && rm -rf "$WORK/idx$n"
  done
  # in-between points for the fit
  for n in 3000 30000 300000 3000000 30000000 300000000; do
    run pack_index idx "$n" "$WORK/idx$n" --lookups 20000
    rm -rf "$WORK/idx$n"
  done
  # many packs at a fixed total of 100M objects: lookups probe every pack
  for p in 1 4 16 64 256; do
    run pack_index_multi idx 100000000 "$WORK/idxm$p" --packs $p --lookups 5000
    rm -rf "$WORK/idxm$p"
  done
  ;;
graph)
  for n in 3000 10000 30000 100000 300000 1000000 3000000 10000000 30000000 100000000; do
    run commit_graph cgraph "$n" "$WORK/cg"
    rm -rf "$WORK/cg"
  done
  for n in 1000 3000 10000 30000 100000 300000 1000000 3000000 10000000; do
    run commit_graph_build cgbuild "$n"
  done
  run commit_graph cgraph 300000000 "$WORK/cg"; rm -rf "$WORK/cg"
  ;;
other)
  run bloom bloom
  for n in 1000 10000 100000 1000000 10000000 100000000 200000000; do
    run agi5 agi5 "$n" "$WORK/a"; rm -rf "$WORK/a"
  done
  for n in 1000 10000 100000 1000000 10000000 100000000 1000000000; do
    run reach reach "$n" "$WORK/r"; rm -rf "$WORK/r"
  done
  for n in 1000 10000 100000 1000000 10000000 100000000; do
    run pfilter pfilter "$n" "$WORK/pf"; rm -rf "$WORK/pf"
  done
  for n in 100 1000 10000 100000 1000000; do
    run manifest manifest "$n" "$WORK/m"; rm -rf "$WORK/m"
  done
  for n in 100 1000 10000 100000 1000000 10000000; do
    run locks locks "$n"
  done
  ;;
esac
