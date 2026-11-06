#!/usr/bin/env bash
# Group commit off/on at 512 writers through vcs-server (docs/research/benchmark-audit.md #4); wrapper for bench_groupcommit_fair.py.
#   usage: scripts/bench_groupcommit_fair.sh <workdir> <out.json> [writers=512] [pushes=2] [runs=3] [seed_zones=512]
set -euo pipefail
[ "$(uname -s)" = Linux ] || { echo "refusing: run on a Linux box, not this laptop" >&2; exit 2; }
exec python3 "$(dirname "$0")/bench_groupcommit_fair.py" "$@"
