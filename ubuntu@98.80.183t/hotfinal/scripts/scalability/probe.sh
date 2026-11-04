#!/bin/bash
# usage: probe.sh kind n   (run on box E, under ~/rt/scalability-probe)
V=~/rt/scalability/target/release/vcs
kind=$1; n=$2
W=~/rt/scalability-probe/$kind-$n; cd $W
t(){ local s=$(date +%s.%N); "$@" >/dev/null 2>&1; local rc=$?; echo "$(echo "$(date +%s.%N)-$s"|bc) s rc=$rc : $*"; }
cd r
rss(){ /usr/bin/time -f "%e s %M KB maxrss : $*" "$@" >/dev/null 2>$W/t.err; tail -1 $W/t.err; }
rss $V status
rss $V log --oneline
rss $V log --oneline -n 10
rss $V tag
rss $V rev-list HEAD
rss $V merge-base HEAD HEAD~1
rss $V ls-files
rss $V gc --dry-run
rss $V gc
rss $V status
du -sh .vcs | cut -f1 | sed 's/^/.vcs size /'
find .vcs -type f | wc -l | sed 's/^/.vcs files /'
