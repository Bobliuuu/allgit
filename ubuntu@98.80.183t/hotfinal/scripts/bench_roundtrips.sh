#!/bin/sh
# Requests and sequential depth per operation against `latsrv` (vcs-server over a local dir with an
# injected per-backend-call latency; EMULATED object store, not real S3).
# usage: scripts/bench_roundtrips.sh <bin-dir> <work-dir> <latency-ms>
set -eu
B=$1; W=$2; L=$3
export PATH="$B:$PATH" VCS_TOKEN=tok1 VCS_PRINCIPAL=bench VCS_AUTHOR_NAME=b VCS_AUTHOR_EMAIL=b@x
rm -rf "$W"; mkdir -p "$W/root"; cd "$W"
echo "bench write * $(vcs-server hash-token tok1)" > tokens
PORT=${PORT:-18771}; URL=http://127.0.0.1:$PORT
srv() { # latency trace
  latsrv --root "$W/root" --tokens "$W/tokens" --listen 127.0.0.1:$PORT --latency-ms "$1" --trace "$2" 2>/dev/null &
  SP=$!; sleep 0.5
}
stop() { kill $SP 2>/dev/null || true; wait $SP 2>/dev/null || true; }
summ() { python3 - "$1" "$2" <<'PY'
import sys
ev=[l.rstrip("\n").split("\t",3) for l in open(sys.argv[1])] if __import__("os").path.exists(sys.argv[1]) else []
ev=[(o,int(s),int(e),k) for o,s,e,k in ev]
ev.sort(key=lambda x:x[1]); best=[]
for i,(o,s,e,k) in enumerate(ev):
    best.append(1+max([best[j] for j in range(i) if ev[j][2]<=s] or [0]))
print(f"{sys.argv[2]:28s} requests={len(ev):3d} depth={max(best) if best else 0:3d}")
if len(sys.argv)>3: pass
PY
}
run() { name=$1; shift; T="$W/trace.$name"; rm -f "$T"; srv "$L" "$T"; s=$(date +%s%N); "$@" >/dev/null 2>&1 || echo "FAILED: $name"; e=$(date +%s%N); stop
  printf "%s wall=%dms\n" "$(summ "$T" "$name")" $(( (e-s)/1000000 )); }
# fixture (no latency)
mkdir src && cd src && vcs init >/dev/null
for z in a b c d; do mkdir -p $z; vcs zone create $z --path $z >/dev/null; done
for i in 1 2 3 4 5 6 7 8; do for z in a b c d; do echo "$z $i" > $z/f$i.txt; done; done
vcs add . >/dev/null && vcs commit -m init >/dev/null
for i in 1 2 3 4 5 6 7 8 9 10; do echo "e$i" >> a/f1.txt; echo "e$i" >> b/f1.txt; vcs add a b >/dev/null; vcs commit -m c$i >/dev/null; done
vcs remote set $URL >/dev/null
srv 0 /dev/null; vcs publish >/dev/null; stop
cd "$W"
run clone vcs clone $URL "$W/c1" --stats
run zone_clone vcs clone $URL "$W/c2" --for a/f1.txt --stats
# new commit pushed, then fetch from a warm clone
cd src && echo more >> a/f1.txt && vcs add a >/dev/null && vcs commit -m n >/dev/null
cd "$W"
run push sh -c "cd '$W/src' && vcs publish"
cd "$W/c2"
run fetch vcs fetch --stats
run noop_fetch vcs fetch --stats
run sync vcs sync
