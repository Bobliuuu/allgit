#!/usr/bin/env bash
# What runs after the soak matrix: gc after the block-cache fix, hot branch, big file against git, limits.
set -uo pipefail
export BASE=/data/run/base VCS_BIN=/data/target/release; S=/data/src/scripts/stress
until grep -q "finished d256-sharded" /data/run/runs/progress.log 2>/dev/null; do sleep 10; done
$S/gc_ops.sh $BASE /data/run/gcfix > /data/run/gcfix.out 2>&1
$S/run_hot.sh /data/run/hot > /data/run/hot.out 2>&1
$S/bigfile_cmp.sh $BASE /data/run/bigcmp $BASE/seed/big/big03.bin > /data/run/bigcmp.out 2>&1
$S/limits.sh /data/run/limits > /data/run/limits.out 2>&1
echo all-done > /data/run/post.done
