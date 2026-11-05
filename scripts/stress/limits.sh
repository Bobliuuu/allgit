#!/usr/bin/env bash
# The two known limits of docs/known-limits.md that the stress base does not reach:
#   1. 1,000,000 commits (gc / fsck / log memory and time): histgen on a small tree, import-git, then fsck and gc
#   2. 10,000,000 files: add, commit, first and second status
#   limits.sh WORKDIR [COMMITS=1000000] [FILES=10000000]    env: VCS_BIN
set -uo pipefail
W=${1:?workdir}; C=${COMMITS:-1000000}; N=${FILES:-10000000}; HERE=$(cd "$(dirname "$0")" && pwd)
export PATH=${VCS_BIN:?VCS_BIN}:$PATH VCS_AUTHOR_NAME=l VCS_AUTHOR_EMAIL=l@x
export XDG_CACHE_HOME=$W/cache; mkdir -p "$W"; cd "$W"; L=$W/limits.tsv; : > "$L"
m() { local op=$1; shift; /usr/bin/time -f "$op\t%e\t%U\t%S\t%M\t%x" -a -o "$L" "$@" > "$W/$op.out" 2> "$W/$op.err" || true; }
# ---- 1M commits on a 20k-file tree
m gen_stream_${C}c bash -c "python3 $HERE/../histgen.py --commits $C --dirs 20 --subdirs 20 --files 50 --lines 6 > hist.stream"
rm -rf h; vcs init h > /dev/null; cd h
m import_${C}c vcs import-git ../hist.stream
m rev_list_first vcs log -n 5 --oneline -- hot/app.txt
m fsck_${C}c vcs fsck
m gc_${C}c vcs gc --dry-run
m log_all_count bash -c 'vcs log -n 100000000 --oneline | wc -l'
cd "$W"; rm -rf h hist.stream
# ---- 10M files
gen10m() { local per=$((N / 16)); for k in $(seq 0 15); do python3 $HERE/../huge_gen_tree.py "$W/t10m" nested $N $((k * per)) $(((k + 1) * per)) & done; wait; }
export -f gen10m; export W N HERE; m gen_${N}files bash -c gen10m
cd "$W/t10m" && vcs init . > /dev/null
m add_${N}files vcs add .
m commit_${N}files vcs commit -m big
m status_first vcs status
m status_second vcs status
du -sk "$W/t10m/.vcs" | awk '{print "store_kb\t"$1}' >> "$L"
cat "$L"
