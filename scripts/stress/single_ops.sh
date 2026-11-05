#!/usr/bin/env bash
# Single-client operations on the stress base repository, vcs (through a vcs-server) against git (same history, same
# stream), one run per cell, quiet box. Writes TSV rows: tool, op, wall_s, user_s, sys_s, max_rss_kb, rc.
#   single_ops.sh BASEDIR OUTDIR        BASEDIR = build_base.sh WORKDIR (base-store/, gitbase.git/, tokens)
set -uo pipefail
B=${1:?base dir}; O=${2:?out dir}; BIN=${VCS_BIN:?VCS_BIN}; export PATH=$BIN:$PATH
export VCS_AUTHOR_NAME=bench VCS_AUTHOR_EMAIL=b@x VCS_TOKEN=tok-admin VCS_PRINCIPAL=admin
mkdir -p "$O"; R=$O/results.tsv; : > "$R"
# the client cache (~/.cache/vcs) must live on the fast disk: on a 3000-IOPS root volume it throttles a 1M-file clone
export XDG_CACHE_HOME=${XDG_CACHE_HOME:-$O/cache}; rm -rf "$XDG_CACHE_HOME"
m() { # tool op cmd...
  local tool=$1 op=$2; shift 2
  /usr/bin/time -f "$tool\t$op\t%e\t%U\t%S\t%M\t%x" -a -o "$R" "$@" >"$O/$tool-$op.out" 2>"$O/$tool-$op.err" || true
}
rm -rf "$O/store" "$O/vc" "$O/vauto" "$O/vslice" "$O/gc"
cp -a "$B/base-store" "$O/store"
vcs-server --root "$O/store" --tokens "$B/tokens" --listen 127.0.0.1:8492 --max-connections 4096 >"$O/server.log" 2>&1 &
SP=$!; until curl -sf http://127.0.0.1:8492/healthz >/dev/null; do sleep 0.2; done
U=http://127.0.0.1:8492
# ---- vcs
m vcs clone_root vcs clone "$U" "$O/vc" --zone root
m vcs clone_auto vcs clone "$U" "$O/vauto" --auto
m vcs clone_slice_z0 vcs clone "$U" "$O/vslice" --zone z0
( cd "$O/vc"
  m vcs status_first vcs status
  m vcs status_second vcs status
  m vcs log_path vcs log -n 50 --oneline -- hot/app.txt
  m vcs log_100 vcs log -n 100 --oneline
  m vcs blame vcs blame hot/app.txt
  m vcs fsck_clone vcs fsck
  m vcs gc_local vcs gc --dry-run )
( cd "$O/vslice"; m vcs gc_remote_dry vcs gc --remote --dry-run )
kill $SP; wait $SP 2>/dev/null
# fsck / gc of the server store as a directory remote (what an operator runs on the server box)
mkdir -p "$O/gc"; ( cd "$O/gc"; vcs init . >/dev/null; vcs remote set "$O/store" >/dev/null
  m vcs gc_remote_sweep_dry vcs gc --remote --dry-run
  m vcs gc_remote_compact vcs gc --remote --compact --yes
  m vcs gc_remote_sweep vcs gc --remote --yes --grace-secs 14400 --hold-secs 600 )
rm -rf "$O/store"
"$(dirname "$0")/git_ops.sh" "$B" "$O"
