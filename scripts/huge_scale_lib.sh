#!/usr/bin/env bash
# Helpers for the huge-scale runs (docs/research/huge-scale.md). source me.
# m <scenario> <tool> <op> <cmd...>: run cmd, append "scenario tool op wall user sys maxrss_kb rc" to $OUT.
OUT=${OUT:-$HOME/data/results.tsv}
export GIT_AUTHOR_NAME=b GIT_AUTHOR_EMAIL=b@x.invalid GIT_COMMITTER_NAME=b GIT_COMMITTER_EMAIL=b@x.invalid
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
VCS=${VCS:-$HOME/vcs}
m() {
  local sc=$1 tool=$2 op=$3; shift 3
  local t; t=$(mktemp)
  ${GTIME:-/usr/bin/time} -f "%e\t%U\t%S\t%M\t%x" -o "$t" "$@" >"$t.out" 2>"$t.err"
  printf "%s\t%s\t%s\t%s\n" "$sc" "$tool" "$op" "$(tail -1 "$t")" | tee -a "$OUT"
  tail -2 "$t.err" | head -c 300 >&2
  rm -f "$t" "$t.out" "$t.err"
}
