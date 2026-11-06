#!/usr/bin/env bash
# Fair `status` pair (docs/research/benchmark-audit.md #2); thin wrapper, logic in bench_status_fair.py (git fsmonitor+untrackedCache,
# vcs watcher off/on, daemon CPU counted, 1% dirty phase, box lock per measured run). REMOTE LINUX BOX ONLY.
#   usage: scripts/bench_status_fair.sh <workdir> <out.json> [runs=9] [files=100000]     (honours CARGO_TARGET_DIR)
set -euo pipefail
[ "$(uname -s)" = Linux ] || { echo "refusing: run on a Linux box, not this laptop" >&2; exit 2; }
exec python3 "$(dirname "$0")/bench_status_fair.py" "$@"
