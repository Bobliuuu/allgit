#!/bin/bash
# Clone/fetch benchmark across local backends (file, RustFS S3, vcs-server) for the storebench workloads.
# Usage: scripts/clone_bench.sh <workdir> <bin-dir> <rustfs-binary> [runs] [workloads...]
# Output: one tab-separated line per (workload, backend, scenario): median wall ms, requests, bytes down.
# Needs: a release build of vcs, vcs-server, storebench, s3bench in <bin-dir>; git for the file-remote baseline.
set -eu
W=$(realpath -m "$1"); BIN=$(realpath "$2"); RUSTFS=$(realpath "$3"); RUNS=${4:-5}; shift 4 || true
WLS=${*:-"zone_subscription deep_history large_monorepo binary_heavy"}
mkdir -p "$W"; cd "$W"
export AWS_ACCESS_KEY_ID=benchak AWS_SECRET_ACCESS_KEY=benchsecret1 AWS_REGION=us-east-1
export VCS_AUTHOR_NAME=bench VCS_AUTHOR_EMAIL=b@x VCS_TIMESTAMP=1700000000 GIT_AUTHOR_NAME=b GIT_AUTHOR_EMAIL=b@x GIT_COMMITTER_NAME=b GIT_COMMITTER_EMAIL=b@x
PIDS=()
cleanup() { for p in "${PIDS[@]:-}"; do kill "$p" 2>/dev/null || true; done; }
trap cleanup EXIT
if [ ! -f up ]; then
  mkdir -p "$W/rfdata"
  RUSTFS_ACCESS_KEY=benchak RUSTFS_SECRET_KEY=benchsecret1 "$RUSTFS" server --address 127.0.0.1:19100 "$W/rfdata" >rustfs.log 2>&1 & PIDS+=($!)
  sleep 4
  "$BIN/s3bench" http://127.0.0.1:19100 clonebench us-east-1 s3b.json --max-requests 60 --max-mb 2 --create-bucket >/dev/null 2>&1 || true
fi
TOK=tok-bench
"$BIN/vcs-server" hash-token $TOK > tokhash
echo "bench write * $(cat tokhash)" > tokens
mkdir -p srvroot
"$BIN/vcs-server" --root "$W/srvroot" --tokens "$W/tokens" --listen 127.0.0.1:19200 >srv.log 2>&1 & PIDS+=($!)
sleep 1

med() { sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}'; }
now() { date +%s%N; }

for wl in $WLS; do
  src=$W/src-$wl; rm -rf "$src" repo-$wl; "$BIN/storebench" gen "$wl" "$src" --scale smoke >/dev/null
  zones=$(sed -n 's/.*"name": "\([^"]*\)".*/\1/p' "$src/..storebench-manifest.json")
  zpaths=$(sed -n 's/.*"path": "\([^"]*\)".*/\1/p' "$src/..storebench-manifest.json")
  rm -f "$src/..storebench-manifest.json"
  repo=$W/repo-$wl; mkdir -p "$repo"; cp -r "$src/." "$repo/"; rm -rf "$src"
  ( cd "$repo"; "$BIN/vcs" init >/dev/null
    paste <(echo "$zones") <(echo "$zpaths") | while read n p; do if [ -n "$n" ]; then "$BIN/vcs" zone create "$n" --path "$p" >/dev/null; fi; done
    "$BIN/vcs" add . >/dev/null; "$BIN/vcs" commit -m init >/dev/null
    if [ "$wl" = deep_history ]; then
      f=$(find . -type f -not -path "./.vcs/*" | sort | sed -n 1p); g=$(find . -type f -not -path "./.vcs/*" | sort | sed -n 2p)
      for i in $(seq 1 999); do echo "line $i" >> "$f"; [ $((i%3)) = 0 ] && echo "x $i" >> "$g"; "$BIN/vcs" add . >/dev/null; VCS_TIMESTAMP=$((1700000000+i)) "$BIN/vcs" commit -m "c$i" >/dev/null; done
    fi
  )
  # git baseline (file remote): same tree, same number of commits
  rm -rf git-$wl gitsrc-$wl; mkdir gitsrc-$wl; cp -r "$repo/." gitsrc-$wl/; rm -rf gitsrc-$wl/.vcs
  if [ "$wl" = deep_history ]; then
    # rebuild the original tree for git's first commit by replaying: simplest is to commit the final tree once per replayed edit
    ( cd gitsrc-$wl; git init -q -b main; git add -A; git commit -q -m init
      f=$(find . -type f -not -path "./.git/*" | sort | sed -n 1p)
      for i in $(seq 1 999); do echo "line $i" >> "$f"; git commit -q -a -m "c$i"; done )
  else
    ( cd gitsrc-$wl; git init -q -b main; git add -A; git commit -q -m init )
  fi
  git clone -q --bare gitsrc-$wl git-$wl; git -C git-$wl config uploadpack.allowFilter true
  # remotes
  fileurl=$W/file-$wl; rm -rf "$fileurl"; mkdir -p "$fileurl"
  s3url="s3://clonebench/$wl-$$?endpoint=http://127.0.0.1:19100"
  srvurl="http://127.0.0.1:19200"
  # each remote needs its own copy of the repo: publish skips what the repo believes is already published
  for be in file s3 http; do
    case $be in file) u=$fileurl;; s3) u=$s3url;; http) u=$srvurl;; esac
    rm -rf "$repo-$be"; cp -r "$repo" "$repo-$be"
    if [ $be = http ]; then rm -rf srvroot/*; export VCS_TOKEN=$TOK; fi
    ( cd "$repo-$be"; "$BIN/vcs" remote set "$u" >/dev/null; "$BIN/vcs" publish >/dev/null )
    rm -rf "$repo-$be"
  done
  nz=$(echo "$zones" | grep -c . || true)
  scen=("full:"); sparse=""
  if [ "$wl" = zone_subscription ]; then k=$((nz/10)); for z in $(echo "$zones" | head -$k); do sparse="$sparse --zone $z"; done; scen+=("sparse10:$sparse"); fi
  if [ "$wl" = large_monorepo ]; then z=$(echo "$zones" | head -1); scen+=("sparse1:--zone $z"); fi
  for be in file s3 http; do
    case $be in file) url=$fileurl;; s3) url=$s3url;; http) url=$srvurl;; esac
    for s in "${scen[@]}"; do
      name=${s%%:*}; args=${s#*:}
      ts=(); rq=0; by=0
      for r in $(seq 1 "$RUNS"); do
        rm -rf cl; t0=$(now)
        out=$("$BIN/vcs" clone "$url" cl $args --stats | grep '^requests')
        t1=$(now); ts+=($(( (t1-t0)/1000000 )))
        rq=$(echo "$out" | sed 's/requests: \([0-9]*\),.*/\1/'); by=$(echo "$out" | sed 's/.*downloaded: \([0-9]*\) bytes.*/\1/')
      done
      m=$(printf '%s\n' "${ts[@]}" | med)
      printf '%s\t%s\t%s\tvcs\twall_ms=%s\trequests=%s\tbytes=%s\n' "$wl" "$be" "$name" "$m" "$rq" "$by"
    done
  done
  if [ -d git-$wl ]; then
    ts=(); for r in $(seq 1 "$RUNS"); do rm -rf gcl; t0=$(now); git clone -q file://$W/git-$wl gcl; t1=$(now); ts+=($(( (t1-t0)/1000000 ))); done
    printf '%s\tfile\tfull\tgit\twall_ms=%s\n' "$wl" "$(printf '%s\n' "${ts[@]}" | med)"
    if [ "$wl" = zone_subscription ]; then
      ts=(); dirs=$(paste <(echo "$zones") <(echo "$zpaths") | head -$((nz/10)) | cut -f2 | tr '\n' ' ')
      for r in $(seq 1 "$RUNS"); do rm -rf gcl; t0=$(now); git clone -q --filter=blob:none --sparse file://$W/git-$wl gcl; (cd gcl; git sparse-checkout set $dirs); t1=$(now); ts+=($(( (t1-t0)/1000000 ))); done
      printf '%s\tfile\tsparse10\tgit\twall_ms=%s\n' "$wl" "$(printf '%s\n' "${ts[@]}" | med)"
    fi
  fi
done
