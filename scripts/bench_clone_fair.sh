#!/usr/bin/env bash
# Fair clone pair for the 1M-file claim (docs/research/benchmark-audit.md #1); thin wrapper, logic in bench_clone_fair.py.
# REMOTE LINUX BOX ONLY (needs ~30 GB and a quiet disk). Cold = fadvise(DONTNEED) of the source repos (no global drop_caches).
#   usage: scripts/bench_clone_fair.sh <srctree|-> <workdir> <out.json> [runs=3] [files=1000000]    (honours CARGO_TARGET_DIR)
set -euo pipefail
[ "$(uname -s)" = Linux ] || { echo "refusing: run on a Linux box, not this laptop" >&2; exit 2; }
exec python3 "$(dirname "$0")/bench_clone_fair.py" "$@"
