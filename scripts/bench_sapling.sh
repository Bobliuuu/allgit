#!/usr/bin/env bash
# Interleaved benchmark: git vs vcs vs Sapling (sl, Git-backed store) on the datasets of scripts/bench_scale.sh.
#
# usage: scripts/bench_sapling.sh <flat|deep|binary> [n=100000] [runs=5]
#   env: SL_BIN (default ~/rt/slbench/dl/x/sl), VCS_BIN, DATAGEN, BENCH_TMP, MIN_FREE_GB (default 8), TOOLS (default "git vcs sl")
# All tools' working copies exist at the same time; for each cell, run i of every tool is executed in turn
# (tool order rotates with i) so machine-load drift hits all tools alike. Prep is untimed.
# Per run we record wall ms and CPU ms (user+sys of the command and every child it waited for).
# Sapling mode: `sl init --git` (Git-backed store: .sl/store/git is a bare Git repo, "git-store" in store/requires).
set -euo pipefail

LAYOUT="${1:-flat}"; N="${2:-100000}"; RUNS="${3:-5}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SL_BIN="${SL_BIN:-$HOME/rt/slbench/dl/x/sl}"
VCS_BIN="${VCS_BIN:-$HOME/rt/slbench/bin/vcs}"
DATAGEN="${DATAGEN:-$HOME/rt/slbench/bin/datagen}"
MIN_FREE_GB="${MIN_FREE_GB:-8}"
PARENT="${BENCH_TMP:-$HOME/rt/slbench/work}"; mkdir -p "$PARENT"
WORK="$(mktemp -d "$PARENT/slscale.XXXXXX")"
TOOLS_LIST=(${TOOLS:-git vcs sl})
trap 'rm -rf "$WORK"' EXIT

export GIT_AUTHOR_NAME=b GIT_AUTHOR_EMAIL=b@x.invalid GIT_COMMITTER_NAME=b GIT_COMMITTER_EMAIL=b@x.invalid
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null
export VCS_AUTHOR_NAME=b VCS_AUTHOR_EMAIL=b@x.invalid
# wrapper so that the username survives `bash -c` (no config file is touched)
mkdir -p "$WORK/bin"; printf '#!/bin/sh\nexec %s --config "ui.username=b <b@x.invalid>" "$@"\n' "$SL_BIN" > "$WORK/bin/sl"; chmod +x "$WORK/bin/sl"
SL="$WORK/bin/sl"

free_gb() { df -k "$WORK" | awk 'NR==2{printf "%.2f", $4/1048576}'; }
guard() { local f; f="$(free_gb)"; if awk -v f="$f" -v m="$MIN_FREE_GB" 'BEGIN{exit !(f<m)}'; then echo "ABORT: free ${f} GiB" >&2; exit 3; fi; }
load() { cut -d" " -f1 /proc/loadavg; }

echo "## Environment"
echo "date:     $(date -u +%FT%TZ)"
echo "uname:    $(uname -a)"
echo "cpu:      $(grep -m1 "model name" /proc/cpuinfo | cut -d: -f2- | xargs)  cores=$(nproc)  mem=$(awk '/MemTotal/{printf "%.0f", $2/1048576}' /proc/meminfo)GiB  fs=$(df -T "$WORK" | awk 'NR==2{print $2}')"
echo "git:      $(git --version)"
echo "vcs:      $("$VCS_BIN" --version) ($VCS_BIN)"
echo "sl:       $($SL_BIN --version | head -1) ($SL_BIN)"
echo "dataset:  layout=$LAYOUT files=$N seed=42 (datagen default); runs per cell=$RUNS; WARM cache; tools=${TOOLS_LIST[*]}"
echo "load1 at start: $(load)   free disk: $(free_gb) GiB"
echo

# timeit CMD -> "wall_ms cpu_ms"
timeit() { python3 -I -c 'import subprocess,sys,time,resource
r0=resource.getrusage(resource.RUSAGE_CHILDREN);t=time.perf_counter()
p=subprocess.run(["bash","-c",sys.argv[1]],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
w=(time.perf_counter()-t)*1000;r1=resource.getrusage(resource.RUSAGE_CHILDREN)
c=((r1.ru_utime-r0.ru_utime)+(r1.ru_stime-r0.ru_stime))*1000
print("%.1f %.1f %d"%(w,c,p.returncode))' "$1"; }

declare -A INIT ADD COMMIT STATUS LOG BR SWS SWM RESET STORE
INIT[git]="git init -q -b main";           ADD[git]="git add .";            COMMIT[git]="git commit -q -m c"
INIT[vcs]="$VCS_BIN init";                 ADD[vcs]="$VCS_BIN add .";       COMMIT[vcs]="$VCS_BIN commit -m c"
INIT[sl]="$SL init --git -q";              ADD[sl]="$SL add -q .";          COMMIT[sl]="$SL commit -q -m c"
STATUS[git]="git status --porcelain";      STATUS[vcs]="$VCS_BIN status";   STATUS[sl]="$SL status"
LOG[git]="git log --oneline -n 50";        LOG[vcs]="$VCS_BIN log -n 50"
LOG[sl]="$SL log -l 50 -T '{node|short} {desc|firstline}\n'"
BR[git]="git branch side";                 BR[vcs]="$VCS_BIN branch side";  BR[sl]="$SL bookmark main; $SL bookmark -i side"
SWS[git]="git checkout -q side";           SWM[git]="git checkout -q main"
SWS[vcs]="$VCS_BIN switch side";           SWM[vcs]="$VCS_BIN switch main"
SWS[sl]="$SL goto -q side";                SWM[sl]="$SL goto -q main"
RESET[git]="rm -rf .git"; RESET[vcs]="rm -rf .vcs"; RESET[sl]="rm -rf .sl"
STORE[git]=.git; STORE[vcs]=.vcs; STORE[sl]=.sl

declare -A W CLONESRC CLONECMD
RAWW="$WORK/raw"; mkdir -p "$RAWW"

# cell NAME PREP_FN(tool) CMD_FN(tool): interleaved over tools.
cell() {
  local name="$1" prep="$2" cmdf="$3" i t k order l0 l1 out
  declare -A WALL CPU LMIN LMAX
  for t in "${TOOLS_LIST[@]}"; do WALL[$t]=""; CPU[$t]=""; done
  for ((i = 0; i < RUNS; i++)); do
    order=(); for ((k = 0; k < ${#TOOLS_LIST[@]}; k++)); do order+=("${TOOLS_LIST[$(( (i + k) % ${#TOOLS_LIST[@]} ))]}"); done
    for t in "${order[@]}"; do
      guard; cd "${W[$t]}"
      $prep "$t" >/dev/null 2>&1 || echo "prep failed: $name $t" >&2
      l0="$(load)"
      out="$(timeit "$($cmdf "$t")")"
      set -- $out
      [ "$3" = 0 ] || echo "WARN: $name $t exit $3" >&2
      WALL[$t]+="$1 "; CPU[$t]+="$2 "
      LMIN[$t]="${LMIN[$t]:-$l0}"; LMAX[$t]="$l0"
    done
  done
  for t in "${TOOLS_LIST[@]}"; do
    printf '| %-24s | %-4s | wall: %s| med %s | cpu: %s| med %s | load1 %s..%s |\n' "$name" "$t" \
      "${WALL[$t]}" "$(printf '%s\n' ${WALL[$t]} | sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}')" \
      "${CPU[$t]}" "$(printf '%s\n' ${CPU[$t]} | sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}')" \
      "${LMIN[$t]}" "${LMAX[$t]}"
  done
}

p_reset_init()     { bash -c "${RESET[$1]}; ${INIT[$1]}"; }
p_reset_init_add() { bash -c "${RESET[$1]}; ${INIT[$1]}; ${ADD[$1]}"; }
p_none()           { true; }
p_sw_side()        { bash -c "${SWS[$1]}"; }
p_clone_clean()    { rm -rf "$WORK/clone-$1"; }
c_add()      { echo "${ADD[$1]}"; }
c_commit()   { echo "${COMMIT[$1]}"; }
c_status()   { echo "${STATUS[$1]}"; }
c_log()      { echo "${LOG[$1]}"; }
c_sw_main()  { echo "${SWM[$1]}"; }
c_clone()    { echo "${CLONECMD[$1]}"; }

modify_1pct() {
  python3 -I - "$1" <<'E'
import os, sys
tag = sys.argv[1]
fs = []
for d, ds, files in os.walk("."):
    ds[:] = [x for x in ds if x not in (".git", ".vcs", ".sl")]
    fs += [os.path.join(d, f) for f in files]
fs.sort()
for i, p in enumerate(fs[::100]):
    with open(p, "wb") as fh:
        fh.write((f"modified {tag} {i}\n").encode() * 8)
print(len(fs[::100]))
E
}
touch_one() {
  python3 -I - "$1" <<'E'
import os, sys
k = int(sys.argv[1])
fs = []
for d, ds, files in os.walk("."):
    ds[:] = [x for x in ds if x not in (".git", ".vcs", ".sl")]
    fs += [os.path.join(d, f) for f in files]
fs.sort()
with open(fs[k * 997 % len(fs)], "wb") as fh:
    fh.write(f"history {k}\n".encode())
E
}

for t in "${TOOLS_LIST[@]}"; do W[$t]="$WORK/$t"; mkdir -p "${W[$t]}"; guard; "$DATAGEN" "$LAYOUT" "$N" "${W[$t]}" >/dev/null; done
echo "(datasets generated; free $(free_gb) GiB; load1 $(load))" >&2

echo "## Results (ms; raw runs in run order, median, load1 sampled before each run: min..max)"
echo "| workflow | tool | wall ms | median | cpu ms | median | load |"
echo "|---|---|---|---|---|---|---|"
cell "add (fresh index)"   p_reset_init     c_add
cell "commit (all staged)" p_reset_init_add c_commit

# committed state for the remaining workflows
for t in "${TOOLS_LIST[@]}"; do cd "${W[$t]}"; bash -c "${RESET[$t]}; ${INIT[$t]}; ${ADD[$t]}; ${COMMIT[$t]}" >/dev/null 2>&1 || echo "build failed $t" >&2; done
echo; echo "## Disk (du -sk of the store dir, KiB)"
for t in "${TOOLS_LIST[@]}"; do cd "${W[$t]}"; echo "after first commit: $t ${STORE[$t]} $(du -sk "${STORE[$t]}" | cut -f1)"; done
echo "working tree: $(cd "${W[${TOOLS_LIST[0]}]}" && du -sk --exclude=.git --exclude=.vcs --exclude=.sl . | cut -f1) KiB (du -sk)"
sleep 3
echo; echo "## Results (continued)"
cell "status (clean)" p_none c_status
for t in "${TOOLS_LIST[@]}"; do cd "${W[$t]}"; bash -c "${BR[$t]}" >/dev/null 2>&1 || echo "branch failed $t" >&2; done
for t in "${TOOLS_LIST[@]}"; do cd "${W[$t]}"; n=$(modify_1pct a); echo "(modified $n files in $t)" >&2; done
cell "status (1% modified)" p_none c_status
echo "status line counts (1% modified): $(for t in "${TOOLS_LIST[@]}"; do cd "${W[$t]}"; echo -n "$t=$(bash -c "${STATUS[$t]}" | wc -l) "; done)"
for t in "${TOOLS_LIST[@]}"; do cd "${W[$t]}"; bash -c "${ADD[$t]}; ${COMMIT[$t]}" >/dev/null 2>&1 || echo "commit2 failed $t" >&2
  for k in $(seq 1 48); do touch_one "$k"; bash -c "${ADD[$t]}; ${COMMIT[$t]}" >/dev/null 2>&1; done
  echo "history length $t: $(bash -c "$(echo "${LOG[$t]}" | sed -e 's/-n 50/-n 1000/' -e 's/-l 50/-l 1000/')" 2>/dev/null | wc -l | tr -d ' ') log lines (git/vcs: commits; sl: template lines)"; done
cell "log (50 commits)" p_none c_log
cell "checkout (1% differ)" p_sw_side c_sw_main

# clone sources: a bare Git repo for git and sl (sl pushes its own history to it); publish dir for vcs
for t in "${TOOLS_LIST[@]}"; do cd "${W[$t]}"
  case $t in
    git) git clone -q --bare . "$WORK/src-git" 2>/dev/null; git -C "$WORK/src-git" repack -adq; CLONECMD[git]="git clone -q file://$WORK/src-git $WORK/clone-git" ;;
    vcs) "$VCS_BIN" remote set "$WORK/src-vcs" >/dev/null 2>&1; "$VCS_BIN" publish >/dev/null 2>&1 || echo "vcs publish failed" >&2
         CLONECMD[vcs]="$VCS_BIN clone $WORK/src-vcs $WORK/clone-vcs" ;;
    sl) git init -q --bare -b main "$WORK/src-sl" 2>/dev/null; $SL paths --add origin "$WORK/src-sl" >/dev/null 2>&1
        $SL bookmark main -f -r . >/dev/null 2>&1; $SL goto -q main >/dev/null 2>&1
        $SL push --to main -r main origin >/dev/null 2>&1 || echo "sl push failed" >&2
        CLONECMD[sl]="$SL clone --git -q $WORK/src-sl $WORK/clone-sl" ;;
  esac; done
W2="$WORK"; for t in "${TOOLS_LIST[@]}"; do W[$t]="$W2"; done   # clone cell runs from $WORK
cell "clone (50 commits)" p_clone_clean c_clone
echo; echo "## Disk after history and clone (KiB)"
for t in "${TOOLS_LIST[@]}"; do
  rm -rf "$WORK/clone-$t"; bash -c "${CLONECMD[$t]}" >/dev/null 2>&1
  echo "clone $t: files=$(find "$WORK/clone-$t" -type f -not -path '*/.git/*' -not -path '*/.vcs/*' -not -path '*/.sl/*' | wc -l)  du -sk total=$(du -sk "$WORK/clone-$t" | cut -f1)"
  rm -rf "$WORK/clone-$t"
done
for t in "${TOOLS_LIST[@]}"; do cd "$WORK/$t"; echo "store after 50 commits: $t ${STORE[$t]} $(du -sk "${STORE[$t]}" | cut -f1)"; done
for d in src-git src-vcs src-sl; do [ -e "$WORK/$d" ] && echo "clone source $d: $(du -sk "$WORK/$d" | cut -f1)"; done
echo; echo "free disk at end: $(free_gb) GiB; load1 at end: $(load)"
