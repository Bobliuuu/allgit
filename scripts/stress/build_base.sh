#!/usr/bin/env bash
# Build the base repository for the monorepo stress run, then publish it to a vcs-server store.
#   build_base.sh WORKDIR [FILES_DIRS=100 SUBDIRS=100 FILES=100 COMMITS=200000 NBIG=20 NZ=32]
# Layout under WORKDIR: stream (fast-import stream), seed/ (the vcs repo, working tree materialised),
# base-store/ (server store holding everything, the starting point of every soak run), gitbase.git (same history in git),
# big-v{1,2,3}.json (sha256 of every big file per version), build.log (phase timings).
# Needs VCS_BIN (dir with vcs, vcs-server) on PATH-able form and python3.
set -euo pipefail
W=${1:?workdir}; D=${DIRS:-100}; SD=${SUBDIRS:-100}; F=${FILES:-100}; C=${COMMITS:-200000}; NBIG=${NBIG:-20}; NZ=${NZ:-32}
HERE=$(cd "$(dirname "$0")" && pwd); BIN=${VCS_BIN:?VCS_BIN}; export PATH=$BIN:$PATH
export VCS_AUTHOR_NAME=seed VCS_AUTHOR_EMAIL=seed@stress.test VCS_TOKEN=tok-admin VCS_PRINCIPAL=admin
mkdir -p "$W"; cd "$W"; L=$W/build.log
t() { local n=$1; shift; local s=$(date +%s.%N); /usr/bin/time -f "$n maxrss_kb=%M user=%U sys=%S" -a -o "$L" "$@"; echo "$n wall=$(echo "$(date +%s.%N) - $s" | bc)" >> "$L"; }
: > "$L"
t gen_stream bash -c "python3 $HERE/../histgen.py --commits $C --dirs $D --subdirs $SD --files $F --lines 6 > stream"
ls -l stream >> "$L"
rm -rf seed; vcs init seed >/dev/null; cd seed
t import_git bash -c "vcs import-git ../stream"
git_t() { :; }
# zones for the writers, the lockable art zone and the big-file zone, on top of the imported root zone
for z in $(seq 0 $((NZ-1))); do vcs zone create z$z --path z$z >/dev/null; mkdir -p z$z; echo "zone $z" > z$z/README; done
vcs zone create art --path art >/dev/null; mkdir -p art; echo '*.bin lockable' > art/.vcsattributes
for k in $(seq 0 15); do head -c 5000 /dev/urandom > art/f$k.bin; done
vcs zone create big --path big >/dev/null; mkdir -p big
t big_gen python3 $HERE/bigfiles.py gen big $NBIG
t big_hash_v1 python3 $HERE/bigfiles.py hash big ../big-v1.json
t add_all vcs add .
t commit_v1 vcs commit -m "zones, art and big files v1"
for v in 2 3; do
  t big_mutate_v$v python3 $HERE/bigfiles.py mutate big $v
  t big_hash_v$v python3 $HERE/bigfiles.py hash big ../big-v$v.json
  t add_big_v$v vcs add big
  t commit_big_v$v vcs commit -m "big files v$v"
done
cd "$W"; echo "admin admin * $(vcs-server hash-token tok-admin)" > tokens
rm -rf base-store; vcs-server --root base-store --tokens tokens --listen 127.0.0.1:8491 --max-connections 4096 >server-base.log 2>&1 &
SP=$!; until curl -sf http://127.0.0.1:8491/healthz >/dev/null; do sleep 0.2; done
cd seed; vcs remote set http://127.0.0.1:8491 >/dev/null
t publish_all vcs publish
kill $SP; wait $SP 2>/dev/null || true
du -sk "$W/base-store" | awk '{print "base_store_kb="$1}' >> "$L"
cd "$W"; rm -rf gitbase.git; git init -q --bare gitbase.git
t git_fast_import bash -c "git -C gitbase.git fast-import --quiet < stream"
du -sk gitbase.git | awk '{print "gitbase_kb="$1}' >> "$L"
cat "$L"
