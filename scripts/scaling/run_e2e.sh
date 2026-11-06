#!/usr/bin/env bash
# Real end-to-end points, vcs vs git on identical streams. Env: VCS_BIN, WORK (NVMe scratch), OUT.
set -u
OUT=${OUT:-docs/research/data/scaling}; WORK=${WORK:-/data/e2e-work}
here=$(dirname "$0")
mkdir -p "$OUT"
for spec in "$@"; do   # e.g. deep:100000 wide:1000000
  kind=${spec%%:*}; n=${spec##*:}
  echo "== $kind $n $(date -u +%T)"
  python3 "$here/e2e.py" "$kind" "$n" "$WORK/$kind$n" "$OUT/e2e.jsonl" --timeout "${TIMEOUT:-5400}" > "$OUT/e2e_$kind$n.log" 2>&1
done
