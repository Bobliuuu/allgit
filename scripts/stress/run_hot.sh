#!/usr/bin/env bash
# Hot shared branch: N protocol-level writers (crates/vcs-bench writerbench, no CLI process per push) pushing one
# zone/branch through vcs-server, with and without the per-branch ticket queue, single and sharded manifest.
# The store is a copy of the stress base store (EMPTY=1: an empty store). Every run is audited by writerbench itself: the
# commit chain behind zone `shared`, branch `main` must have grown by exactly the number of acked pushes.
#   run_hot.sh OUTDIR     env: BASE, VCS_BIN, CLIENTS="64 128 256", PUSHES=30, EMPTY
# summary.tsv columns: layout, ticket, clients, acked, conflicts, unknowns, errors, wall_s, pushes/s, p50 ms, p95 ms, p99 ms, audit
set -uo pipefail
O=${1:?outdir}; BASE=${BASE:?BASE}; export PATH=${VCS_BIN:?VCS_BIN}:$PATH
CLIENTS=${CLIENTS:-"64 128 256"}; PUSHES=${PUSHES:-30}
export XDG_CACHE_HOME=$O/cache; mkdir -p "$O"
for layout in single sharded; do
  cum=0
  rm -rf "$O/store"; if [ -n "${EMPTY:-}" ]; then mkdir -p "$O/store"; else cp -a "$BASE/base-store" "$O/store"; fi
  [ $layout = sharded ] && vcs-server migrate-sharded --backend "$O/store" > "$O/migrate-$layout.txt" 2>&1
  for ticket in no yes; do
    for n in $CLIENTS; do
      tag=$layout-ticket_$ticket-n$n
      vcs-server --root "$O/store" --tokens "$BASE/tokens" --listen 127.0.0.1:8493 --max-connections 4096 --rate-limit 1000000,1000000 > "$O/$tag.server.log" 2>&1 &
      SP=$!; until curl -sf http://127.0.0.1:8493/healthz >/dev/null; do sleep 0.2; done
      ulimit -n 1048576 2>/dev/null
      # a fresh branch per run: the zone name encodes the run so earlier runs do not count
      /usr/bin/time -f "%e s wall, %U user, %S sys, %M KB" -o "$O/$tag.time" \
        writerbench http://127.0.0.1:8493 tok-admin --mode same --clients $n --pushes $PUSHES --tag $tag $( [ $ticket = yes ] && echo --ticket ) --decorrelated > "$O/$tag.json" 2> "$O/$tag.err"
      ps -o rss= -p $SP > "$O/$tag.server_rss_kb"
      kill $SP; wait $SP 2>/dev/null
      python3 - "$O/$tag.json" >> "$O/summary.tsv" <<PY
import json,sys
r=json.load(open(sys.argv[1])); l=sorted(r["lat_ms"])
p=lambda q: l[min(len(l)-1,int(q*len(l)))] if l else float("nan")
audit="ok" if r["chain_after"]-r["chain_before"]==r["acked"] else "MISMATCH %d vs %d"%(r["chain_after"]-r["chain_before"],r["acked"])
print("\t".join(map(str,["$layout","$ticket",$n,r["acked"],r["conflicts"],r["unknowns"],len(r["errors"]),round(r["wall_s"],1),round(r["acked"]/max(r["wall_s"],1e-9),1),round(p(.5),1),round(p(.95),1),round(p(.99),1),audit])))
PY
    done
  done
done
echo done
