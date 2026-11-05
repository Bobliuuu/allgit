#!/usr/bin/env bash
# One monorepo stress run: restore the base store, start vcs-server, run N writers + readers (+ chaos) with
# scripts/soak/soak.py, record sar (cpu, disk, network) next to the soak output.
#   run_stress.sh NAME N MINUTES [extra soak.py run args...]
# env: BASE (build_base.sh workdir), RUNS (output root), VCS_BIN, NZ (zones, default 32)
set -uo pipefail
NAME=${1:?name}; N=${2:?clients}; MIN=${3:?minutes}; shift 3
BASE=${BASE:?BASE}; RUNS=${RUNS:?RUNS}; export VCS_BIN=${VCS_BIN:?VCS_BIN}
export SOAK_NZ=${NZ:-32} SOAK_HOT=1 PATH=$VCS_BIN:$PATH
HERE=$(cd "$(dirname "$0")" && pwd); OUT=$RUNS/$NAME
mkdir -p "$RUNS"; rm -rf "$OUT"   # soak.py recreates OUT, so side logs live next to it until the end
# client cache (~/.cache/vcs) on the fast disk, cold at the start of every run
export XDG_CACHE_HOME=${XDG_CACHE_HOME:-$RUNS/cache-$NAME}; rm -rf "$XDG_CACHE_HOME"
ulimit -n 1048576 2>/dev/null || true
sar -u -d -n DEV 10 > "$OUT.sar.txt" 2>/dev/null & SAR=$!
( while sleep 5; do echo "$(date +%s) $(awk '/ lo:/{print $2, $10}' /proc/net/dev) $(df -k --output=used "$RUNS" | tail -1)"; done > "$OUT.lo-bytes.txt" ) & LO=$!
python3 "$HERE/../soak/soak.py" run --kind vcs --base-store "$BASE/base-store" --slice --bigmb 128 --n "$N" --minutes "$MIN" \
  --think 4 --ramp 90 --out "$OUT" --big-manifest "$BASE/big-v3.json" "$@" 2>&1 | tee "$OUT.soak.log"
kill $SAR $LO 2>/dev/null
mv "$OUT.soak.log" "$OUT/soak.log"; mv "$OUT.sar.txt" "$OUT/sar.txt"; mv "$OUT.lo-bytes.txt" "$OUT/lo-bytes.txt"
python3 "$HERE/../soak/report.py" "$OUT" --json "$OUT/report.json" > "$OUT/report.txt" 2>&1
echo finished "$NAME" >> "$RUNS/progress.log"
