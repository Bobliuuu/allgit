#!/usr/bin/env bash
# One-box driver: (1) hot-path scoreboard groups clean/dirty/small vs git, (2) zone-profile autosync benchmark.
# usage: scripts/bench_autosync_hotpath.sh OUTDIR     (run from the repo root after `cargo build --release -p vcs-cli -p vcs-server -p datagen`)
# env: MAXLOAD (default 1.0) - each timed phase waits (up to 10 min) until load1 is below it.
# Records the build SHA and load1 around every phase. Timed parts run under flock ~/bench.lock.
set -euo pipefail
OUT="$(cd "$(mkdir -p "${1:?outdir}" && echo "$1")" && pwd)"; MAXLOAD="${MAXLOAD:-1.0}"
T="$PWD/target/release"; W="${BENCH_TMP:-$HOME/asw}"; mkdir -p "$W"
load() { cut -d' ' -f1 /proc/loadavg; }
quiet() { for _ in $(seq 120); do awk -v l="$(load)" -v m="$MAXLOAD" 'BEGIN{exit !(l<m)}' && return 0; sleep 5; done; echo "load never below $MAXLOAD: $(load)" >&2; return 1; }
{ echo "sha=$(git rev-parse HEAD 2>/dev/null || cat BUILD_SHA)"; echo "vcs=$($T/vcs --version)"; echo "git=$(git --version)"; echo "date=$(date -u +%FT%TZ)"; echo "cores=$(nproc)"; } > "$OUT/meta.txt"
python3 -I scripts/command_scoreboard.py setup --work "$W/sb" --vcs $T/vcs --datagen $T/datagen --n 100000
for g in clean dirty small; do
  quiet; echo "$g load_start=$(load)" >> "$OUT/meta.txt"
  flock ~/bench.lock python3 -I scripts/command_scoreboard.py run --work "$W/sb" --vcs $T/vcs --group $g --runs 9 --out "$OUT/hot-$g.json"
  echo "$g load_end=$(load)" >> "$OUT/meta.txt"
done
python3 -I scripts/bench_zoneprofile.py --phase gen --work "$W/zp" --vcs $T/vcs --server $T/vcs-server
quiet; echo "autosync load_start=$(load)" >> "$OUT/meta.txt"
flock ~/bench.lock python3 -I scripts/bench_zoneprofile.py --phase run --work "$W/zp" --vcs $T/vcs --server $T/vcs-server --runs 5
echo "autosync load_end=$(load)" >> "$OUT/meta.txt"
cp "$W/zp/result.json" "$OUT/autosync.json"
