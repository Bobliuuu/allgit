#!/bin/bash
# Local disjoint-writer bench: N writers (zones z<i>) through vcs-server over a disk backend, single manifest
# vs sharded manifest (after `vcs-server migrate-sharded`). LOCAL numbers (loopback, local disk), one run per cell.
# usage: shard_bench.sh <target-release-dir> single|sharded <N> <pushes-per-writer>
set -e
BIN=$1; MODE=$2; N=$3; K=$4
W=$(mktemp -d); trap 'kill $SP 2>/dev/null; rm -rf $W' EXIT
ulimit -n 8192 2>/dev/null || true
T=bench-token
echo "bench write * $($BIN/vcs-server hash-token $T)" > $W/tokens
PORT=$((20000 + RANDOM % 20000))
start() { $BIN/vcs-server --backend $W/repo --tokens $W/tokens --listen 127.0.0.1:$PORT --max-connections 4096 $SERVER_FLAGS >/dev/null 2>$W/server.log & SP=$!; sleep 1; }
start
# seed: one push so a manifest exists, and a few hundred zones so the sharded repo has real fan-out
$BIN/writerbench http://127.0.0.1:$PORT $T --mode zones --clients $N --pushes 1 --first-client 100000 --tag seed >/dev/null
if [ "$MODE" = sharded ]; then
  kill $SP; wait $SP 2>/dev/null || true
  $BIN/vcs-server migrate-sharded --backend $W/repo
  start
fi
$BIN/writerbench http://127.0.0.1:$PORT $T --mode zones --clients $N --pushes $K --tag run > $W/out.json
python3 - "$W/out.json" "$MODE" "$N" "$K" <<'PY'
import json,sys
d=json.load(open(sys.argv[1])); lat=sorted(d["lat_ms"])
p=lambda q: lat[min(len(lat)-1,int(q*len(lat)))] if lat else None
print(json.dumps({"mode":sys.argv[2],"N":int(sys.argv[3]),"acked":d["acked"],"of":int(sys.argv[3])*int(sys.argv[4]),"wall_s":d["wall_s"],
 "pps":round(d["acked"]/d["wall_s"],1),"p50_ms":p(.5),"p95_ms":p(.95),"p99_ms":p(.99),"conflicts":d["conflicts"],"errors":d["errors"][:2]}))
PY
