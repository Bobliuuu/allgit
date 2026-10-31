#!/usr/bin/env bash
# Compare equivalent workflows: git vs vcs on the same deterministic dataset.
#
# usage: scripts/bench_vs_git.sh [layout=flat|deep|bigdir] [n=1000] [runs=5]
# Uses `hyperfine` if installed, else a simple wall-clock loop (min/median of `runs`).
# Builds `vcs` in release mode unless VCS_BIN is set. No numbers are baked in here.
set -euo pipefail

LAYOUT="${1:-flat}"
N="${2:-1000}"
RUNS="${3:-5}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

if [ -z "${VCS_BIN:-}" ]; then
  (cd "$ROOT" && cargo build --release -q -p vcs-cli -p vcs-bench)
  VCS_BIN="$ROOT/target/release/vcs"
fi
DATAGEN="$ROOT/target/release/datagen"
[ -x "$DATAGEN" ] || (cd "$ROOT" && cargo build --release -q -p vcs-bench)

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
SRC="$WORK/src"
"$DATAGEN" "$LAYOUT" "$N" "$SRC" >/dev/null

export GIT_AUTHOR_NAME=b GIT_AUTHOR_EMAIL=b@x.invalid GIT_COMMITTER_NAME=b GIT_COMMITTER_EMAIL=b@x.invalid
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null

echo "## Environment"
echo "date:     $(date -u +%FT%TZ)"
echo "uname:    $(uname -a)"
case "$(uname -s)" in
  Darwin) echo "cpu:      $(sysctl -n machdep.cpu.brand_string)  cores=$(sysctl -n hw.ncpu)  mem=$(( $(sysctl -n hw.memsize) / 1073741824 ))GiB" ;;
  *)      echo "cpu:      $(grep -m1 'model name' /proc/cpuinfo | cut -d: -f2- | xargs)  cores=$(nproc)" ;;
esac
echo "fs:       $(df -T "$WORK" 2>/dev/null | tail -1 || df "$WORK" | tail -1)  (mount: $(mount | grep -F " $(df "$WORK" | tail -1 | awk '{print $NF}') " | head -1))"
echo "git:      $(git --version)"
echo "vcs:      $("$VCS_BIN" --version 2>/dev/null || echo "$VCS_BIN") (release build)"
echo "dataset:  layout=$LAYOUT files=$N seed=42 (scripts + datagen are deterministic)"
echo "timer:    $(command -v hyperfine >/dev/null && echo hyperfine || echo 'builtin loop')  runs=$RUNS"
echo "cache:    WARM (files just written; OS page cache not dropped; no root-free portable way to drop it)."
echo

# Times CMD with python's perf_counter around subprocess only (interpreter start-up excluded).
timeit() { python3 -c 'import subprocess,sys,time
t=time.perf_counter();subprocess.run(["bash","-c",sys.argv[1]],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);print((time.perf_counter()-t)*1000)' "$1"; }

# time_cmd NAME PREP CMD : PREP runs untimed before every run; CMD is timed.
declare -a RESULTS
time_cmd() {
  local name="$1" prep="$2" cmd="$3"
  if command -v hyperfine >/dev/null; then
    local j="$WORK/hf.json"
    hyperfine -N --shell=none -r "$RUNS" --prepare "bash -c '$prep'" --export-json "$j" "bash -c '$cmd'" >/dev/null 2>&1 || true
    RESULTS+=("$name|$(python3 -c "import json;r=json.load(open('$j'))['results'][0];print('%.1f|%.1f'%(r['median']*1e3,r['min']*1e3))")")
  else
    local ts=() i
    for ((i = 0; i < RUNS; i++)); do
      bash -c "$prep" >/dev/null 2>&1
      ts+=("$(timeit "$cmd")")
    done
    RESULTS+=("$name|$(printf '%s\n' "${ts[@]}" | sort -n | python3 -c "import sys;v=[float(x) for x in sys.stdin];print('%.1f|%.1f'%(v[len(v)//2],v[0]))")")
  fi
}

# Workflows. Each tool gets its own copy of the dataset.
for tool in git vcs; do
  W="$WORK/$tool"; mkdir -p "$W"; cp -R "$SRC/." "$W/"
  if [ "$tool" = git ]; then
    BIN=git; INIT="git init -q"; ADD="git add ."; COMMIT="git commit -q -m c"; STATUS="git status --porcelain"; LOG="git log --oneline"
    RESET="rm -rf .git"
  else
    BIN="$VCS_BIN"; INIT="$VCS_BIN init"; ADD="$VCS_BIN add ."; COMMIT="$VCS_BIN commit -m c"; STATUS="$VCS_BIN status"; LOG="$VCS_BIN log"
    RESET="rm -rf .vcs"
  fi
  cd "$W"
  time_cmd "$tool init"          "$RESET"                                   "$INIT"
  time_cmd "$tool add . (fresh)" "$RESET; $INIT"                            "$ADD"
  time_cmd "$tool commit (all)"  "$RESET; $INIT; $ADD"                      "$COMMIT"
  # state for status/log: committed repo
  bash -c "$RESET; $INIT; $ADD; $COMMIT" >/dev/null 2>&1
  time_cmd "$tool status clean"  "true"                                     "$STATUS"
  # 1% modified: rewrite every 100th file in place (same bytes length class, new content)
  i=0; for f in $(find . -type f -name '*.txt' -not -path './.git/*' -not -path './.vcs/*' | sort); do
    if [ $((i % 100)) -eq 0 ]; then echo "modified $i" >"$f"; fi; i=$((i + 1)); done
  time_cmd "$tool status 1% mod" "true"                                     "$STATUS"
  time_cmd "$tool log"           "true"                                     "$LOG"
  cd "$ROOT"
done

echo "## Results (ms, lower is better; median and min of $RUNS runs)"
printf '| %-22s | %10s | %10s |\n' workflow median min
printf '|%s|%s|%s|\n' "$(printf -- '-%.0s' {1..24})" "$(printf -- '-%.0s' {1..12})" "$(printf -- '-%.0s' {1..12})"
for r in "${RESULTS[@]}"; do IFS='|' read -r n m mn <<<"$r"; printf '| %-22s | %10s | %10s |\n' "$n" "$m" "$mn"; done
cat <<'EOF'

## Semantic differences (read before comparing)
- git `add .` honours .gitignore; vcs has no ignore support in this dataset (none present, so equal work).
- git packs/zlib-compresses objects; vcs writes loose objects (see docs/benchmarks.md for format).
- git `commit` of a root tree hashes with SHA-1/SHA-256; vcs uses BLAKE3 (different hashing cost).
- vcs `commit` with a staged tree rewrites whole tree objects (no subtree skipping yet); git reuses the index cache-tree.
- git `status` uses the index stat cache incl. fsmonitor/untracked cache if enabled (disabled here via empty config);
  vcs `status` stat-caches but rehashes racy entries (mtime within granularity of the index write).
- `log` prints a one-commit history here; this script does not compare deep-history traversal.
- Process start-up (exec + repo discovery) is included in every timing for both tools.
- Cache state: WARM only. Cold-cache numbers would require purging the page cache (root).
EOF
