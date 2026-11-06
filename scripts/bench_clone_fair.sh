#!/usr/bin/env bash
# Fair clone pair for the 1M-file claim (docs/research/benchmark-audit.md #1). REMOTE BOX ONLY (needs ~25 GB and a quiet disk).
#   usage: scripts/bench_clone_fair.sh <srctree> <workdir> [runs=3]    srctree = output of scripts/huge_gen_tree.py <dest> nested 1000000
# git arms: (a) loose source, --no-hardlinks (what huge_s1.sh timed)  (b) after `git gc`, file:// clone  (c) after `git gc`, local path, --no-hardlinks
# vcs arm : clone of a published local-path remote. Drops the page cache between runs when root (cold) and also reports warm.
set -euo pipefail
[ "$(uname -s)" = Linux ] || { echo "refusing: run on a Linux box, not this laptop" >&2; exit 2; }
SRC=${1:?srctree}; W=${2:?workdir}; N=${3:-3}; ROOT=$(cd "$(dirname "$0")/.." && pwd); VCS=$ROOT/target/release/vcs
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_AUTHOR_NAME=b GIT_AUTHOR_EMAIL=b@x.invalid GIT_COMMITTER_NAME=b GIT_COMMITTER_EMAIL=b@x.invalid
mkdir -p "$W"; rm -rf "$W/g" "$W/v" "$W/r"; cp -a "$SRC" "$W/g"; cp -a "$SRC" "$W/v"
tm() { local l=$1; shift; local s; s=$(date +%s.%N); "$@" >/dev/null 2>&1; echo "$l $(echo "$(date +%s.%N) - $s" | bc)"; }
drop() { [ "$(id -u)" = 0 ] && { sync; echo 3 > /proc/sys/vm/drop_caches; } || true; }
( cd "$W/g" && git init -q && git add -A && git commit -qm one )
( cd "$W/v" && "$VCS" init >/dev/null && "$VCS" add . >/dev/null && "$VCS" commit -m one >/dev/null && "$VCS" remote set "$W/r" >/dev/null && "$VCS" publish >/dev/null )
for i in $(seq "$N"); do
  drop; rm -rf "$W/c"; tm git_loose_nohardlinks git clone -q --no-hardlinks "$W/g" "$W/c"
done
( cd "$W/g" && git gc -q )
for i in $(seq "$N"); do
  drop; rm -rf "$W/c"; tm git_packed_file_url git clone -q "file://$W/g" "$W/c"
  drop; rm -rf "$W/c"; tm git_packed_nohardlinks git clone -q --no-hardlinks "$W/g" "$W/c"
  drop; rm -rf "$W/c"; tm vcs_clone "$VCS" clone "$W/r" "$W/c"
done
