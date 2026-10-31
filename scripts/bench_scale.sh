#!/usr/bin/env bash
# Scale benchmark: git vs vcs CLI on one large generated dataset, one tool at a time.
#
# usage: scripts/bench_scale.sh <flat|deep|binary> [n=100000] [runs=5]
#   env: VCS_BIN, DATAGEN (default target/release/...; CARGO_TARGET_DIR is honoured),
#        BENCH_TMP (scratch parent dir), MIN_FREE_GB (abort below this free space, default 4)
# Workflows: add, commit, status (clean / 1% modified), log (50 commits), checkout (switch, 1% differ).
# Output: raw per-run milliseconds and the median per cell, plus machine load before/after each cell.
# The dataset is generated straight into the work dir of the tool under test and deleted afterwards
# (disk is the scarce resource: one working copy plus one object store exist at any moment).
set -euo pipefail

LAYOUT="${1:-flat}"
N="${2:-100000}"
RUNS="${3:-5}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TGT="${CARGO_TARGET_DIR:-$ROOT/target}"
VCS_BIN="${VCS_BIN:-$TGT/release/vcs}"
DATAGEN="${DATAGEN:-$TGT/release/datagen}"
MIN_FREE_GB="${MIN_FREE_GB:-4}"
PARENT="${BENCH_TMP:-${TMPDIR:-/tmp}}"
WORK="$(mktemp -d "$PARENT/vcs-scale.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

export GIT_AUTHOR_NAME=b GIT_AUTHOR_EMAIL=b@x.invalid GIT_COMMITTER_NAME=b GIT_COMMITTER_EMAIL=b@x.invalid
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null

free_gb() { df -k "$WORK" | awk 'NR==2{printf "%.2f", $4/1048576}'; }
guard() {
  local f; f="$(free_gb)"
  if awk -v f="$f" -v m="$MIN_FREE_GB" 'BEGIN{exit !(f<m)}'; then
    echo "ABORT: free space ${f} GiB < ${MIN_FREE_GB} GiB; cleaning up" >&2
    exit 3
  fi
}
load() { if [ -r /proc/loadavg ]; then cut -d" " -f1 /proc/loadavg; else sysctl -n vm.loadavg | tr -d "{}" | awk '{print $1}'; fi; }

echo "## Environment"
echo "date:     $(date -u +%FT%TZ)"
echo "uname:    $(uname -a)"
if [ -r /proc/cpuinfo ]; then echo "cpu:      $(grep -m1 "model name" /proc/cpuinfo | cut -d: -f2- | xargs)  cores=$(nproc)  mem=$(awk '/MemTotal/{printf "%.0f", $2/1048576}' /proc/meminfo)GiB  fs=$(df -T "$WORK" | awk 'NR==2{print $2}')"; fi
[ "$(uname -s)" = Darwin ] && echo "cpu:      $(sysctl -n machdep.cpu.brand_string)  cores=$(sysctl -n hw.ncpu)  mem=$(( $(sysctl -n hw.memsize) / 1073741824 ))GiB"
echo "git:      $(git --version)"
echo "vcs:      $("$VCS_BIN" --version 2>/dev/null || echo "$VCS_BIN") (release build)"
echo "dataset:  layout=$LAYOUT files=$N seed=42; runs per cell=$RUNS; WARM cache"
echo "load1 at start: $(load)   free disk: $(free_gb) GiB"
echo

# timeit CMD: wall ms of `bash -c CMD` (python perf_counter around the subprocess only).
timeit() { python3 -c 'import subprocess,sys,time
t=time.perf_counter();r=subprocess.run(["bash","-c",sys.argv[1]],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);print("%.1f"%((time.perf_counter()-t)*1000), file=sys.stdout);sys.exit(0)' "$1"; }

# cell NAME PREP CMD: PREP untimed before each run; prints raw runs and median.
cell() {
  local name="$1" prep="$2" cmd="$3" ts=() i l0 l1
  l0="$(load)"
  for ((i = 0; i < RUNS; i++)); do
    guard
    bash -c "$prep" >/dev/null 2>&1 || { echo "prep failed for $name" >&2; }
    ts+=("$(timeit "$cmd")")
  done
  l1="$(load)"
  printf '| %-26s | %-8s | %s | %s | load1 %s -> %s |\n' "$name" "$TOOL" \
    "$(printf '%s ' "${ts[@]}")" \
    "$(printf '%s\n' "${ts[@]}" | sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}')" "$l0" "$l1"
}

# Rewrite every 100th file (sorted order) in place with new content; arg1 = tag.
modify_1pct() {
  python3 -I - "$1" <<'E'
import os, sys
tag = sys.argv[1]
fs = []
for d, ds, files in os.walk("."):
    ds[:] = [x for x in ds if x not in (".git", ".vcs")]
    fs += [os.path.join(d, f) for f in files]
fs.sort()
for i, p in enumerate(fs[::100]):
    with open(p, "wb") as fh:
        fh.write((f"modified {tag} {i}\n").encode() * 8)
E
}
# Rewrite exactly file number K (sorted) for the history commits.
touch_one() {
  python3 -I - "$1" <<'E'
import os, sys
k = int(sys.argv[1])
fs = []
for d, ds, files in os.walk("."):
    ds[:] = [x for x in ds if x not in (".git", ".vcs")]
    fs += [os.path.join(d, f) for f in files]
fs.sort()
with open(fs[k * 997 % len(fs)], "wb") as fh:
    fh.write(f"history {k}\n".encode())
E
}

echo "## Results (ms; raw runs, median, load average)"
echo "| workflow                   | tool     | raw runs (ms) | median | load |"
echo "|----------------------------|----------|---------------|--------|------|"

GEN() { "$DATAGEN" "$LAYOUT" "$N" "$1" >/dev/null; }

for TOOL in git vcs; do
  W="$WORK/$TOOL"; mkdir -p "$W"
  guard; GEN "$W"; cd "$W"
  if [ "$TOOL" = git ]; then
    INIT="git init -q -b main"; ADD="git add ."; COMMIT="git commit -q -m c"; STATUS="git status --porcelain"
    LOG="git log --oneline -n 50"; BR="git branch side"; SW_SIDE="git checkout -q side"; SW_MAIN="git checkout -q -"
    ADDALL="git add -A"; RESET="rm -rf .git"; STORE=.git
  else
    INIT="$VCS_BIN init"; ADD="$VCS_BIN add ."; COMMIT="$VCS_BIN commit -m c"; STATUS="$VCS_BIN status"
    LOG="$VCS_BIN log -n 50"; BR="$VCS_BIN branch side"; SW_SIDE="$VCS_BIN switch side"; SW_MAIN="$VCS_BIN switch main"
    ADDALL="$VCS_BIN add ."; RESET="rm -rf .vcs"; STORE=.vcs
  fi
  [ "$TOOL" = git ] && SW_MAIN="git checkout -q main"
  cell "add (fresh index)"   "$RESET; $INIT"            "$ADD"
  cell "commit (all staged)" "$RESET; $INIT; $ADD"      "$COMMIT"
  # committed state for the remaining workflows
  bash -c "$RESET; $INIT; $ADD; $COMMIT" >/dev/null 2>&1
  echo "(store size after first commit, $TOOL: $(du -sk "$STORE" | cut -f1) KiB; free $(free_gb) GiB)" >&2
  sleep 3
  cell "status (clean)"      "true"                     "$STATUS"
  bash -c "$BR" >/dev/null 2>&1
  modify_1pct a
  cell "status (1% modified)" "true"                    "$STATUS"
  bash -c "$ADDALL; $COMMIT" >/dev/null 2>&1          # commit 2 on main (1% differs from side)
  for k in $(seq 1 48); do touch_one "$k"; bash -c "$ADDALL; $COMMIT" >/dev/null 2>&1; done
  cell "log (50 commits)"    "true"                     "$LOG"
  cell "checkout (1% differ)" "$SW_SIDE"                "$SW_MAIN"
  cd "$WORK"; rm -rf "$W"
done
echo
echo "free disk at end: $(free_gb) GiB; load1 at end: $(load)"
