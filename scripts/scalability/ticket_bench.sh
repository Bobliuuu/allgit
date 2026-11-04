#!/bin/bash
# Local one-branch contention bench: N writers on ONE branch through vcs-server over a disk backend,
# with and without the per-branch ticket queue. LOCAL numbers (loopback, local disk), one run per cell.
# usage: ticket_bench.sh <target-release-dir> <N> <pushes-per-writer> [--ticket] [extra writerbench flags]
set -e
BIN=$1; N=$2; K=$3; shift 3
W=$(mktemp -d); trap 'kill $SP 2>/dev/null; rm -rf $W' EXIT
ulimit -n 8192 2>/dev/null || true
T=bench-token
echo "bench write * $($BIN/vcs-server hash-token $T)" > $W/tokens
PORT=$((20000 + RANDOM % 20000))
$BIN/vcs-server --backend $W/repo --tokens $W/tokens --listen 127.0.0.1:$PORT --max-connections 4096 >/dev/null 2>$W/server.log &
SP=$!
sleep 1
START=$(date +%s.%N)
$BIN/writerbench http://127.0.0.1:$PORT $T --mode same --clients $N --pushes $K "$@" > $W/out.json
python3 - "$W/out.json" "$N" "$K" "$*" <<'PY'
import json,sys
d=json.load(open(sys.argv[1])); lat=sorted(d["lat_ms"])
p=lambda q: lat[min(len(lat)-1,int(q*len(lat)))] if lat else None
print(json.dumps({"flags":sys.argv[4],"N":int(sys.argv[2]),"acked":d["acked"],"of":int(sys.argv[2])*int(sys.argv[3]),"wall_s":d["wall_s"],
 "pps":round(d["acked"]/d["wall_s"],1),"p50_ms":p(.5),"p95_ms":p(.95),"p99_ms":p(.99),"conflicts":d["conflicts"],"errors":d["errors"][:2]}))
PY
