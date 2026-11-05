#!/usr/bin/env bash
# Hot shared branch: N protocol-level writers (crates/vcs-bench writerbench, no CLI process per push) pushing one
# zone/branch through vcs-server, with and without the per-branch ticket queue, single and sharded manifest.
# The store is a copy of the stress base store. Every acked push is audited: the chain behind zone `shared`, branch
# `main` must hold exactly the acked commits.
#   run_hot.sh OUTDIR     env: BASE, VCS_BIN, CLIENTS="64 128 256", PUSHES=30
set -uo pipefail
O=${1:?outdir}; BASE=${BASE:?BASE}; export PATH=${VCS_BIN:?VCS_BIN}:$PATH
CLIENTS=${CLIENTS:-"64 128 256"}; PUSHES=${PUSHES:-30}
export XDG_CACHE_HOME=$O/cache; mkdir -p "$O"
for layout in single sharded; do
  cum=0
  rm -rf "$O/store"; cp -a "$BASE/base-store" "$O/store"
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
      acked=$(python3 -c "import json;print(json.load(open('$O/$tag.json'))['acked'])"); cum=$((cum + acked))
      # audit: the chain behind shared/main holds exactly the commits acked so far in this store
      rm -rf "$O/audit"; vcs clone http://127.0.0.1:8493 "$O/audit" --zone shared > /dev/null 2>&1
      got=$(cd "$O/audit" 2>/dev/null && vcs log --zone shared -n 10000000 --oneline 2>/dev/null | wc -l)
      echo -e "$tag\tacked_cum=$cum\tchain=$got" >> "$O/audit.tsv"
      kill $SP; wait $SP 2>/dev/null
      python3 - "$O/$tag.json" >> "$O/summary.tsv" <<PY
import json,sys
r=json.load(open(sys.argv[1])); l=sorted(r["lat_ms"])
p=lambda q: l[min(len(l)-1,int(q*len(l)))] if l else float("nan")
print("\t".join(map(str,["$layout","$ticket",$n,r["acked"],r["conflicts"],r["unknowns"],len(r["errors"]),round(r["wall_s"],1),round(r["acked"]/max(r["wall_s"],1e-9),1),round(p(.5),1),round(p(.95),1),round(p(.99),1)])))
PY
    done
  done
done
echo done
