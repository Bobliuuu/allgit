#!/usr/bin/env python3
"""Summarise bench_other_vcs.py output (stdout JSON lines) as markdown: median wall ms / median CPU ms (n runs)."""
import json, statistics, sys
from collections import defaultdict

CELLS = ["add", "commit", "status_clean", "status_1pct", "log50", "checkout_1pct", "clone"]
data = defaultdict(lambda: defaultdict(list))
fails = defaultdict(list)
sizes, env = {}, {}
for path in sys.argv[1:]:
    for line in open(path):
        if line.startswith("# env"):
            env[path] = json.loads(line[6:])
            continue
        if not line.startswith("{"):
            continue
        d = json.loads(line)
        if d["cell"] == "size_kib":
            sizes[(d["layout"], d["tool"])] = d
        elif d["rc"] == 0 and d["wall_ms"] is not None:
            data[(d["layout"], d["cell"])][d["tool"]].append(d)
        else:
            fails[(d["layout"], d["cell"], d["tool"])].append(d.get("err", "")[:100])
for lay in sorted({k[0] for k in data}):
    print(f"\n### {lay}\n\n| cell | tool | n | median wall ms | median CPU ms | load1 range | raw wall ms |\n|---|---|---|---|---|---|---|")
    for c in CELLS:
        for t, rs in data[(lay, c)].items():
            w = [r["wall_ms"] for r in rs]
            cp = [r["cpu_ms"] for r in rs]
            lo = [r["load0"] for r in rs] + [r["load1"] for r in rs]
            print(f"| {c} | {t} | {len(rs)} | {statistics.median(w):,.0f} | {statistics.median(cp):,.0f} | {min(lo):.1f}-{max(lo):.1f} | {' '.join(f'{x:,.0f}' for x in w)} |")
    for (l, c, t), e in fails.items():
        if l == lay:
            print(f"\nFAILED {c} {t}: {len(e)} runs, first error: {e[0]!r}")
    print("\n| tool | store KiB after commit 1 | store KiB after 50 commits | wc metadata KiB |\n|---|---|---|---|")
    for (l, t), s in sizes.items():
        if l == lay:
            print(f"| {t} | {s['store_after_commit1']:,} | {s['store_after_50']:,} | {s['wc_metadata']:,} |")
