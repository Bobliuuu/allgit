#!/usr/bin/env bash
# Chaos case for #480/#488: SIGKILL a mutating vcs mid-flight, then assert the next command
# proceeds within a bounded time and fsck is clean. Usage: chaos_locks.sh [rounds] (vcs on PATH).
set -euo pipefail
rounds=${1:-20}
d=$(mktemp -d); trap 'rm -rf "$d"' EXIT
cd "$d"; vcs init >/dev/null
for i in $(seq 1 "$rounds"); do
  ( n=0; while :; do n=$((n+1)); echo "v$n" > "f$n"; vcs add "f$n" >/dev/null 2>&1 || true; done ) &
  pid=$!
  sleep "0.$((RANDOM % 9 + 1))"
  pkill -KILL -P "$pid" 2>/dev/null || true; kill -KILL "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true
  echo "r$i" > "after$i"
  start=$(date +%s)
  timeout 10 vcs add "after$i" >/dev/null || { echo "FAIL: repo wedged after kill -9 (round $i)"; exit 1; }
  [ $(( $(date +%s) - start )) -le 5 ] || { echo "FAIL: slow recovery"; exit 1; }
  vcs fsck >/dev/null || { echo "FAIL: fsck (round $i)"; exit 1; }
done
echo "chaos_locks OK ($rounds rounds)"
