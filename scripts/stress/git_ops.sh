#!/usr/bin/env bash
# git side of single_ops.sh (same history, from the same fast-import stream). Appends to OUTDIR/results.tsv.
#   git_ops.sh BASEDIR OUTDIR
set -uo pipefail
B=${1:?base dir}; O=${2:?out dir}; R=$O/results.tsv; mkdir -p "$O"; touch "$R"
m() { # tool op cmd...
  local tool=$1 op=$2; shift 2
  /usr/bin/time -f "$tool\t$op\t%e\t%U\t%S\t%M\t%x" -a -o "$R" "$@" >"$O/$tool-$op.out" 2>"$O/$tool-$op.err" || true
}
git -C "$B/gitbase.git" symbolic-ref HEAD refs/heads/main   # the stream's branch is main, a fresh bare repo points at master
rm -rf "$O/gclone"
m git clone_full git clone -q --no-hardlinks "$B/gitbase.git" "$O/gclone"
( cd "$O/gclone"
  m git status_first git status --porcelain
  m git status_second git status --porcelain
  m git log_path git log -n 50 --oneline -- hot/app.txt
  m git log_100 git log -n 100 --oneline
  m git blame timeout 1800 git blame hot/app.txt
  m git fsck git fsck --no-dangling
  m git gc git gc -q )
cp -r "$B/gitbase.git" "$O/gitbase-gc.git"
m git gc_bare git -C "$O/gitbase-gc.git" gc -q
m git fsck_bare git -C "$B/gitbase.git" fsck --no-dangling
rm -rf "$O/gitbase-gc.git" "$O/gclone" "$O/vc" "$O/vauto" "$O/vslice" "$O/gc"
cat "$R"
