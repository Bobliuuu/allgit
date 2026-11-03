#!/usr/bin/env python3
"""Print a table from highload.py JSON: scenario, n, arm, pushes/s, p50/p99 push ms, rejects, cpu, rss, verified."""
import json, sys
d = json.load(open(sys.argv[1]))
print("scn      n  arm       push/s  p50ms  p99ms  rej  srvcpu  rssMiB  clone_p50  ok")
for r in d["runs"]:
    p = r.get("push_ms") or {}
    c = (r.get("clone_ms") or {}).get("p50")
    f = lambda x, w=6, n=0: ("-" if x is None else f"{x:{w}.{n}f}")
    print(f"{r['scenario']:8} {r['n']:3} {r['arm']:9} {f(r.get('pushes_per_s'),6,1)} {f(p.get('p50'),6)} {f(p.get('p99'),6)} "
          f"{r.get('rejected_pushes','-'):>4} {f(r.get('srv_cpu_s'),6,1)} {f(r.get('srv_rss_mib'),6)} {f(c,9)}  {r.get('verified')} {r.get('fatal') or ''}")
