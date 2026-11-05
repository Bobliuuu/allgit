#!/usr/bin/env bash
# Remote gc / compaction of a copy of the base store, as an operator runs it on the server box (directory remote):
# sweep dry run, compaction, sweep (real), sweep dry run again. Rows: op, wall_s, user_s, sys_s, max_rss_kb, rc.
#   gc_ops.sh BASEDIR OUTDIR        env: VCS_BIN
set -uo pipefail
B=${1:?base}; O=${2:?out}; export PATH=${VCS_BIN:?VCS_BIN}:$PATH VCS_AUTHOR_NAME=g VCS_AUTHOR_EMAIL=g@x
export XDG_CACHE_HOME=$O/cache; rm -rf "$O"; mkdir -p "$O"; R=$O/results.tsv; : > "$R"
m() { local op=$1; shift; /usr/bin/time -f "$op\t%e\t%U\t%S\t%M\t%x" -a -o "$R" "$@" > "$O/$op.out" 2> "$O/$op.err" || true; }
cp -a "$B/base-store" "$O/store"
mkdir "$O/wd"; cd "$O/wd"; vcs init . > /dev/null; vcs remote set "$O/store" > /dev/null
m sweep_dry_before vcs gc --remote --dry-run
m compact vcs gc --remote --compact --yes
m sweep_dry_after_compact vcs gc --remote --dry-run
m sweep_real vcs gc --remote --yes --grace-secs 14400 --hold-secs 600
cat "$R"; tail -n 2 "$O"/sweep_real.out
rm -rf "$O/store"
