#!/usr/bin/env python3
"""Pareto-frontier scoring for allgit storage benchmarks (issue #181). Stdlib only.

Reads one or more benchmark JSON files (schema allgit-bench/v1, see
docs/research/storage/benchmark-methodology.md) and prints, per (workload, cache) group,
a table of designs with a flag for membership of the Pareto frontier. It never collapses
objectives into one score.

Input:  {"schema": "allgit-bench/v1", "results": [
           {"design": str, "workload": str, "cache": "cold|warm|partial",
            "correct": bool (optional, default true),
            "metrics": {name: number | [trial values...]}}]}
Trial lists are reduced to their median; the relative spread (max-min)/median is shown
as +-% in the table so noisy metrics are visible. Designs with correct=false are
reported as DISQUALIFIED and excluded from the frontier (correctness is a gate, not a score).

Usage:
  storage_score.py bench.json [more.json] --objectives stored_bytes,gets,p95_ms
      [--maximize name,...] [--group-by workload,cache] [--json]
      [--price-class s3_standard]   # adds monthly_cost_usd from the cost model
                                    # (needs metrics stored_bytes, puts, gets, egress_bytes)
"""
import argparse
import json
import os
import statistics
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))


def reduce_metric(v):
    """-> (median, relative spread or None)"""
    if isinstance(v, (list, tuple)):
        if not v:
            raise ValueError("empty trial list")
        med = statistics.median(v)
        spread = None if med == 0 else (max(v) - min(v)) / abs(med)
        return float(med), spread
    return float(v), None


def dominates(a, b, objs, maximize):
    """a dominates b: no worse in all objectives, strictly better in at least one."""
    better = False
    for o in objs:
        x, y = a[o], b[o]
        if o in maximize:
            x, y = -x, -y
        if x > y:
            return False
        if x < y:
            better = True
    return better


def frontier(rows, objs, maximize=()):
    """Return list of (row, dominated_by_names). Rows need a 'values' dict covering objs."""
    out = []
    for r in rows:
        dom = [s["design"] for s in rows if s is not r and dominates(s["values"], r["values"], objs, set(maximize))]
        out.append((r, dom))
    return out


def load(paths, price_class=None):
    rows = []
    pricing = None
    for p in paths:
        with open(p) as f:
            d = json.load(f)
        if d.get("schema") != "allgit-bench/v1":
            raise SystemExit(f"{p}: schema must be allgit-bench/v1")
        for r in d["results"]:
            vals, spreads = {}, {}
            for k, v in r["metrics"].items():
                vals[k], spreads[k] = reduce_metric(v)
            if price_class:
                import storage_cost_model as cm
                pricing = pricing or cm.load_pricing()
                w = {k: vals.get(k, 0) for k in ("stored_bytes", "puts", "gets", "lists", "egress_bytes")}
                vals["monthly_cost_usd"] = cm.run(w, pricing, [price_class])[price_class]["total"]
                spreads["monthly_cost_usd"] = None
            rows.append({"design": r["design"], "workload": r.get("workload", "-"), "cache": r.get("cache", "-"),
                         "correct": r.get("correct", True), "values": vals, "spread": spreads, "source": p})
    return rows


def score(rows, objs, maximize=(), group_by=("workload", "cache")):
    groups = {}
    for r in rows:
        groups.setdefault(tuple(r[g] for g in group_by), []).append(r)
    result = []
    for key in sorted(groups):
        g = groups[key]
        ok = [r for r in g if r["correct"]]
        missing = [r["design"] for r in ok if any(o not in r["values"] for o in objs)]
        if missing:
            raise SystemExit(f"group {key}: designs missing objectives: {missing}")
        fr = frontier(ok, objs, maximize)
        entries = [{"design": r["design"], "on_frontier": not dom, "dominated_by": dom,
                    "values": {o: r["values"][o] for o in objs},
                    "spread": {o: r["spread"].get(o) for o in objs}} for r, dom in fr]
        for r in g:
            if not r["correct"]:
                entries.append({"design": r["design"], "on_frontier": False, "disqualified": True,
                                "dominated_by": [], "values": {o: r["values"].get(o) for o in objs},
                                "spread": {}})
        result.append({"group": dict(zip(group_by, key)), "entries": entries})
    return result


def render(result, objs):
    lines = []
    for g in result:
        lines.append("## " + ", ".join(f"{k}={v}" for k, v in g["group"].items()))
        hdr = ["design"] + list(objs) + ["frontier"]
        rows = [hdr]
        for e in sorted(g["entries"], key=lambda e: (not e["on_frontier"], e["design"])):
            cells = [e["design"]]
            for o in objs:
                v, sp = e["values"][o], e["spread"].get(o)
                cells.append("-" if v is None else (f"{v:g}" + (f" (+-{sp*100:.0f}%)" if sp else "")))
            cells.append("DISQUALIFIED (incorrect)" if e.get("disqualified")
                         else ("yes" if e["on_frontier"] else "dominated by " + ",".join(e["dominated_by"])))
            rows.append(cells)
        w = [max(len(r[i]) for r in rows) for i in range(len(hdr))]
        for r in rows:
            lines.append("| " + " | ".join(r[i].ljust(w[i]) for i in range(len(hdr))) + " |")
        lines.append("")
    return "\n".join(lines)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("files", nargs="+")
    ap.add_argument("--objectives", required=True)
    ap.add_argument("--maximize", default="")
    ap.add_argument("--group-by", default="workload,cache")
    ap.add_argument("--price-class")
    ap.add_argument("--json", action="store_true")
    a = ap.parse_args(argv)
    objs = [o for o in a.objectives.split(",") if o]
    mx = [o for o in a.maximize.split(",") if o]
    rows = load(a.files, a.price_class)
    res = score(rows, objs, mx, tuple(a.group_by.split(",")))
    print(json.dumps(res, indent=2) if a.json else render(res, objs))
    return 0


if __name__ == "__main__":
    sys.exit(main())
