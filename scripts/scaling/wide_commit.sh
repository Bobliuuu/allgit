#!/bin/bash
# Wide-repo one-file commit / status benchmark: one import with $BASE_BIN, then the same steps with each binary.
# usage: wide_commit.sh N WORKDIR BASE_VCS NEW_VCS      (appends rows to WORKDIR/rows.txt; keeps repo until `rm -rf`)
# Timing steps run under `flock /tmp/box-timing.lock` on a shared box.
set -u
N=$1; W=$2; BASE=$3; NEW=$4
HERE=$(cd "$(dirname "$0")" && pwd)
export VCS_AUTHOR_NAME=B VCS_AUTHOR_EMAIL=b@x XDG_CACHE_HOME=$W/cache
mkdir -p "$W"; R=$W/r
T() { # label cmd...
  local label=$1; shift
  flock /tmp/box-timing.lock /usr/bin/time -f "$label %e s %M KB" -o "$W/t.tmp" "$@" > /dev/null 2>&1
  cat "$W/t.tmp" | tee -a "$W/rows.txt"
}
if [ ! -d "$R/.vcs" ]; then
  python3 "$HERE/../scalability/gen.py" wide "$N" > "$W/s.stream"
  "$BASE" init "$R" > /dev/null
  cd "$R"
  flock /tmp/box-timing.lock /usr/bin/time -f "import %e s %M KB" -o "$W/t.tmp" "$BASE" import-git - < "$W/s.stream" > /dev/null
  cat "$W/t.tmp" | tee -a "$W/rows.txt"
  rm -f "$W/s.stream"
fi
cd "$R"
i=0
for BIN in "$BASE" "$NEW"; do
  tag=$(basename "$(dirname "$(dirname "$BIN")")")
  T "$tag status_a" "$BIN" status
  T "$tag status_b" "$BIN" status
  for k in 1 2; do
    echo "edit $tag $k" >> a0/b0/f0.txt
    "$BIN" add a0/b0/f0.txt > /dev/null
    T "$tag commit_$k" "$BIN" commit -m "edit $tag $k"
  done
  T "$tag status_c" "$BIN" status
done
