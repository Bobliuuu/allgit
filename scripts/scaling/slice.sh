#!/usr/bin/env bash
# Slice clone vs full clone on a repository born with zones: N files in 100 zones (a000..a099), all published
# to a directory remote; then `clone --zone z000` (one slice, 1/100 of the files) against the full clone and git.
# usage: slice.sh N WORKDIR OUT.jsonl     (N = files, a multiple of 10000; env VCS_BIN)
set -u
N=${1:?files}; W=${2:?workdir}; OUT=${3:?out}
VCS=${VCS_BIN:-vcs}; here=$(cd "$(dirname "$0")" && pwd)
export VCS_AUTHOR_NAME=b VCS_AUTHOR_EMAIL=b@x XDG_CACHE_HOME=$W/cache
rm -rf "$W"; mkdir -p "$W/r"
row() { # tool step cmd...   (/usr/bin/time: wall user sys maxrss)
  local tool=$1 step=$2; shift 2
  local t0=$(date +%s.%N)
  /usr/bin/time -f "%U %S %M" -o "$W/t" "$@" >/dev/null 2>&1; local rc=$?
  local t1=$(date +%s.%N)
  read -r u s m < "$W/t" || true
  echo "{\"tool\":\"$tool\",\"kind\":\"slice\",\"n\":$N,\"step\":\"$step\",\"wall_s\":$(echo "$t1-$t0" | bc -l | sed 's/^\./0./'),\"user_s\":${u:-null},\"sys_s\":${s:-null},\"maxrss_kb\":${m:-null},\"rc\":$rc}" | tee -a "$OUT"
}
python3 "$here/../huge_gen_tree.py" "$W/r" nested "$N" >/dev/null
cd "$W/r"
$VCS init . >/dev/null
for i in $(seq -w 0 99); do $VCS zone create "z0$i" --path "a0$i" >/dev/null 2>&1 || $VCS zone create "z$i" --path "a0$i" >/dev/null; done
row vcs add_all $VCS add .
row vcs commit_all $VCS commit -m init
mkdir "$W/rem"; $VCS remote set "$W/rem" >/dev/null
row vcs publish_all $VCS publish
ZONE=$($VCS zone list | awk 'NR==2{print $1}')
cd "$W"
row vcs clone_all $VCS clone "$W/rem" "$W/c_all"
row vcs clone_one_zone $VCS clone "$W/rem" "$W/c_z" --zone "$ZONE"
echo "{\"tool\":\"vcs\",\"kind\":\"slice\",\"n\":$N,\"step\":\"files_in_slice\",\"files\":$(find "$W/c_z" -type f -not -path '*/.vcs/*' | wc -l)}" | tee -a "$OUT"
# git: same tree, full clone and a sparse+partial clone of one directory
cd "$W/r" && rm -rf .vcs
git init -q . && git add -A >/dev/null 2>&1 && git commit -q -m init
git init -q --bare "$W/grem.git"; git -C "$W/grem.git" config uploadpack.allowFilter true; git push -q "$W/grem.git" HEAD:main
cd "$W"
row git clone_all git clone -q --no-local "$W/grem.git" "$W/g_all"
row git clone_one_dir git clone -q --no-local --filter=blob:none --sparse "file://$W/grem.git" "$W/g_sparse"
rm -rf "$W"
