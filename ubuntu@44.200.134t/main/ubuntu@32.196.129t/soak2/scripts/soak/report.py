#!/usr/bin/env python3
"""Summarise one soak run directory: per-op latency table, error classes, resource trend.
  report.py RUNDIR [--json OUT]
"""
import collections, csv, glob, json, os, sys


def pct(v, p):
    v = sorted(v)
    return v[min(len(v) - 1, int(p / 100 * len(v)))] if v else float("nan")


def main():
    d = sys.argv[1]
    ops = collections.defaultdict(list)
    kinds = collections.Counter()
    errs = collections.Counter()
    tmin, tmax = 1e18, 0
    for fn in glob.glob(os.path.join(d, "ops-*.jsonl")):
        for l in open(fn):
            r = json.loads(l)
            if r["op"] == "done":
                continue
            tmin, tmax = min(tmin, r["t"]), max(tmax, r["t"])
            kinds[(r["op"], r["k"])] += 1
            if r["k"] == "ok":
                ops[r["op"]].append(r["dt"])
            elif r["k"] == "error":
                m = r.get("err", "")
                # normalise ids
                import re
                m = re.sub(r"[0-9a-f]{12,64}", "<id>", m)
                m = re.sub(r"\d+", "N", m)[:140]
                errs[(r["op"], m)] += 1
    span = max(tmax - tmin, 1)
    out = {"span_s": round(span), "ops": {}, "errors": [], "resources": {}}
    print(f"span {span:.0f}s")
    print("op,n_ok,n_expected_fail,n_error,ops_per_s,p50_ms,p95_ms,p99_ms,max_ms")
    for op in sorted(set(o for o, _ in kinds)):
        ok, ex, er = kinds[(op, "ok")], kinds[(op, "expected")], kinds[(op, "error")]
        v = ops[op]
        row = dict(n_ok=ok, n_expected_fail=ex, n_error=er, per_s=round((ok + ex + er) / span, 2),
                   p50_ms=round(pct(v, 50) * 1000, 1), p95_ms=round(pct(v, 95) * 1000, 1),
                   p99_ms=round(pct(v, 99) * 1000, 1), max_ms=round(max(v) * 1000, 1) if v else None)
        out["ops"][op] = row
        print(f"{op},{ok},{ex},{er},{row['per_s']},{row['p50_ms']},{row['p95_ms']},{row['p99_ms']},{row['max_ms']}")
    for (op, m), n in errs.most_common(25):
        out["errors"].append({"op": op, "msg": m, "n": n})
        print(f"ERR {n}x [{op}] {m}")
    sf = os.path.join(d, "samples.csv")
    if os.path.exists(sf):
        rows = list(csv.DictReader(open(sf)))
        def col(k):
            return [float(r[k]) for r in rows if float(r[k]) >= 0]
        n = len(rows)
        for k in ("rss_kb", "cpu_pct", "fds", "threads", "manifest_bytes", "packs"):
            c = col(k)
            if not c:
                continue
            q = max(1, len(c) // 4)
            out["resources"][k] = dict(first_quartile_mean=round(sum(c[:q]) / q, 1), last_quartile_mean=round(sum(c[-q:]) / q, 1),
                                       max=max(c), last=c[-1])
        print("resources:", json.dumps(out["resources"]))
    if "--json" in sys.argv:
        json.dump(out, open(sys.argv[sys.argv.index("--json") + 1], "w"), indent=1)


main()
