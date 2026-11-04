#!/usr/bin/env bash
# Interleaved, load-robust comparison: git vs vcs (base build) vs vcs (new build).
#
# usage: scripts/bench_compare.sh [--runs N=7] [--taskset CPUS] [--nice N] [--sizes "10000 100000"]
#                                 [--layouts "flat deep binary"] [--out DIR]
#   env: VCS_BIN_BASE (older vcs build, optional), VCS_BIN_NEW (current build; VCS_BIN also accepted),
#        VCS_BIN_WATCH (a build with --features watcher; adds tool "neww": status cells only, `vcs watch` running),
#        DATAGEN, BENCH_TMP, MIN_FREE_GB (default 4)
# One command re-runs the whole matrix, e.g. after perf branches land:
#   VCS_BIN_BASE=/path/vcs-c47fcda VCS_BIN_NEW=target/release/vcs scripts/bench_compare.sh
# Per round every tool runs once in random order (same load for all); wall and CPU (user+sys
# from wait4 rusage) are recorded, with load average and iowait around each round. Ratios are
# paired medians with a bootstrap 95% CI; a cell whose CPU max/min > 2x prints "noisy", no ratio.
# --taskset 2-5 pins every command to cores 2-5 (needs util-linux taskset; only helps if those
# cores are otherwise idle). --nice N lowers priority (positive N) of the benchmarked commands.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
RUNS=7; SIZES="10000 100000"; LAYOUTS="flat deep binary"; OUT=""; EXTRA=()
while [ $# -gt 0 ]; do
  case "$1" in
    --runs) RUNS="$2"; shift 2;;
    --sizes) SIZES="$2"; shift 2;;
    --layouts) LAYOUTS="$2"; shift 2;;
    --out) OUT="$2"; shift 2;;
    --taskset) EXTRA+=(--taskset "$2"); shift 2;;
    --nice) EXTRA+=(--nice "$2"); shift 2;;
    *) echo "unknown arg $1" >&2; exit 2;;
  esac
done
export MIN_FREE_GB="${MIN_FREE_GB:-4}"
echo "## Environment"; date -u +%FT%TZ; uname -a; git --version
[ -r /proc/loadavg ] && { echo "loadavg: $(cat /proc/loadavg)"; echo "other busy procs:"; ps -eo pcpu,comm --sort=-pcpu | head -6; }
for v in VCS_BIN_BASE VCS_BIN_NEW VCS_BIN_WATCH; do [ -n "${VCS_BIN:-}" ] || true; b="${!v:-}"; [ -n "$b" ] && echo "$v=$b $("$b" --version 2>/dev/null || true)"; done
for n in $SIZES; do for l in $LAYOUTS; do
  [ -n "$OUT" ] && mkdir -p "$OUT" && o=(--out "$OUT/$l-$n.json") || o=()
  python3 -I "$HERE/bench_compare.py" "$l" "$n" --runs "$RUNS" --min-free-gb "$MIN_FREE_GB" "${EXTRA[@]}" "${o[@]}"
done; done
