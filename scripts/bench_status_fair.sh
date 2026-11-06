#!/usr/bin/env bash
# Fair `status`/`diff` pair for the audit (docs/research/benchmark-audit.md #2). REMOTE BOX ONLY: builds a 100k-file tree.
#   usage: scripts/bench_status_fair.sh <workdir> [runs=9]     (needs target/release/{vcs,datagen}; git >= 2.37 for the builtin fsmonitor)
# Arms (all warm, interleaved per round, same tree): git default | git fsmonitor+untrackedCache | vcs watcher off | vcs watcher on.
# Reports wall ms per run and, for the watcher arm, the daemon's own CPU (/proc/<pid>/stat) so it is not hidden.
set -euo pipefail
[ "$(uname -s)" = Linux ] || { echo "refusing: run on a Linux box, not this laptop" >&2; exit 2; }
W=${1:?workdir}; N=${2:-9}; ROOT=$(cd "$(dirname "$0")/.." && pwd); VCS=$ROOT/target/release/vcs; DG=$ROOT/target/release/datagen
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_AUTHOR_NAME=b GIT_AUTHOR_EMAIL=b@x.invalid GIT_COMMITTER_NAME=b GIT_COMMITTER_EMAIL=b@x.invalid
mkdir -p "$W"; rm -rf "$W/t"; "$DG" flat 100000 "$W/t" >/dev/null; cd "$W/t"
git init -q . && git add -A && git commit -qm one
"$VCS" init >/dev/null && "$VCS" add . >/dev/null && "$VCS" commit -m one >/dev/null
t() { python3 - "$@" <<'PY'
import subprocess,sys,time,os
env=dict(os.environ); a=sys.argv[1:]
while a and "=" in a[0].split()[0] if a else False: k,v=a.pop(0).split("=",1); env[k]=v
t0=time.perf_counter(); subprocess.run(a,env=env,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL); print(round((time.perf_counter()-t0)*1000,1))
PY
}
echo "load1 at start: $(cut -d' ' -f1 /proc/loadavg)"
git config core.fsmonitor false; git config core.untrackedCache false
for r in $(seq "$N"); do
  echo "git_default $(t git status --porcelain)"
  git config core.fsmonitor true; git config core.untrackedCache true; git status >/dev/null  # prime the builtin daemon
  echo "git_fsmonitor $(t git status --porcelain)"
  git fsmonitor--daemon stop 2>/dev/null || true; git config core.fsmonitor false; git config core.untrackedCache false
  echo "vcs_nowatch $(t VCS_NO_AUTOWATCH=1 VCS_NO_FSMONITOR=1 "$VCS" status)"
  "$VCS" status >/dev/null  # may auto-start `vcs watch`
  echo "vcs_watch $(t "$VCS" status)"
done
for p in $(pgrep -f 'vcs watch' || true); do echo "vcs_watch_daemon_cpu_ticks pid=$p $(awk '{print $14+$15}' /proc/$p/stat)"; done
