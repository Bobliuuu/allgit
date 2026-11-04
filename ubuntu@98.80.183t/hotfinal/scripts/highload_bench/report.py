#!/usr/bin/env python3
"""Win/tie/loss table from highload.py JSON files (several reps per cell).

usage: report.py OUT.md RUN1.json [RUN2.json ...]

Pairs: vcs_disk vs git (both local disk), vcs_s3 vs walgit (both on RustFS).
Per cell the median and the min..max over reps are kept. TIE when the medians are within 10 percent of
each other or the min..max ranges overlap; otherwise WIN/LOSS for vcs by the metric's direction.
Metrics: pushes_per_s (higher is better), push p50 and p99 in ms (lower), clone p50 under load (lower).
"""
import json, statistics, sys
from collections import defaultdict

PAIRS = [("vcs_disk", "git"), ("vcs_s3", "walgit")]
METRICS = [("pushes_per_s", "pushes/s", True), ("push_p50", "push p50 ms", False), ("push_p99", "push p99 ms", False),
           ("clone_p50", "clone p50 ms", False)]


def val(r, m):
    if m == "pushes_per_s":
        return r.get(m)
    if m == "push_p50":
        return (r.get("push_ms") or {}).get("p50")
    if m == "push_p99":
        return (r.get("push_ms") or {}).get("p99")
    if m == "clone_p50":
        return (r.get("clone_ms") or {}).get("p50")


def main():
    cells = defaultdict(list)  # (scenario, n, arm, metric) -> [values]
    ok = defaultdict(lambda: [0, 0])
    for f in sys.argv[2:]:
        for r in json.load(open(f))["runs"]:
            if r.get("fatal"):
                continue
            ok[(r["scenario"], r["n"], r["arm"])][0] += bool(r.get("verified"))
            ok[(r["scenario"], r["n"], r["arm"])][1] += 1
            for m, _, _ in METRICS:
                v = val(r, m)
                if v is not None:
                    cells[(r["scenario"], r["n"], r["arm"], m)].append(v)
    out = ["| scenario | pushers | metric | vcs arm | vcs median (min..max) | other | other median (min..max) | verdict |", "|---|---|---|---|---|---|---|---|"]
    tally = defaultdict(lambda: defaultdict(int))
    keys = sorted({(s, n, m) for (s, n, a, m) in cells})
    for s, n, m in keys:
        label, higher = next((l, h) for mm, l, h in METRICS if mm == m)
        for va, vb in PAIRS:
            a, b = cells.get((s, n, va, m)), cells.get((s, n, vb, m))
            if not a or not b:
                continue
            ma, mb = statistics.median(a), statistics.median(b)
            overlap = min(a) <= max(b) and min(b) <= max(a)
            close = abs(ma - mb) <= 0.10 * max(abs(ma), abs(mb))
            if close or overlap:
                v = "tie"
            else:
                v = "win" if (ma > mb) == higher else "loss"
            tally[(va, vb)][v] += 1
            out.append(f"| {s} | {n or '1'} | {label} | {va} | {ma:.1f} ({min(a):.1f}..{max(a):.1f}) | {vb} | {mb:.1f} ({min(b):.1f}..{max(b):.1f}) | {v} |")
    out.append("")
    for (va, vb), t in tally.items():
        out.append(f"{va} vs {vb}: {t['win']} wins, {t['tie']} ties, {t['loss']} losses")
    out.append("")
    out.append("Cells with verification failures (a push that did not land) or fatal errors: " +
               (", ".join(f"{k[0]}/{k[1]}/{k[2]} ({v[0]}/{v[1]} verified)" for k, v in sorted(ok.items()) if v[0] != v[1]) or "none"))
    open(sys.argv[1], "w").write("\n".join(out) + "\n")
    print("\n".join(out))


if __name__ == "__main__":
    main()
