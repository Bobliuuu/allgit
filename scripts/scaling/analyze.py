#!/usr/bin/env python3
"""Fit growth exponents to the MEASURED sweeps and project to 1e12 (clearly labelled PROJECTED).

usage: analyze.py DATA_DIR            (DATA_DIR = docs/research/data/scaling; reads *.jsonl, writes summary.json, tables.md)

For every (component, op) with >= 3 distinct N we fit  y = c * N^k  by least squares in log-log space over
(a) all points and (b) the largest 4 points ("tail"). Class from the tail exponent k:
  k < 0.15  flat or O(log n) (a binary search over 1e3..1e9 ids grows log2 n = 10 -> 30, k = 0.08)
  k < 0.6   sub-linear
  else      O(n)
Projection to 1e12: flat/log class: y = a + b*log2(N) fitted to the points with N >= 1e5 (optimistic: ignores new
cache/TLB/IO regimes beyond the measured range); otherwise y = y_top * (1e12 / N_top)^k_tail. Every projected number
is an extrapolation of a model, not a measurement.
"""
import collections, glob, json, math, os, sys

d = sys.argv[1]
rows = []
for f in sorted(glob.glob(os.path.join(d, "*.jsonl"))):
    if os.path.basename(f).startswith("e2e"):
        continue
    for line in open(f):
        r = json.loads(line)
        if "comp" in r:
            r["file"] = os.path.basename(f)
            rows.append(r)

# series: (file-group, comp, op) -> {n: row}. pack_index_mmap* files are the fixed reader; keep them apart.
series = collections.defaultdict(dict)
for r in rows:
    series[(r["file"].replace(".jsonl", ""), r["comp"], r["op"])][r["n"]] = r


def fit(xs, ys):
    lx, ly = [math.log(x) for x in xs], [math.log(y) for y in ys]
    n = len(lx)
    mx, my = sum(lx) / n, sum(ly) / n
    sxx = sum((a - mx) ** 2 for a in lx)
    if sxx == 0:
        return None
    k = sum((a - mx) * (b - my) for a, b in zip(lx, ly)) / sxx
    c = math.exp(my - k * mx)
    ss_tot = sum((b - my) ** 2 for b in ly) or 1e-12
    ss_res = sum((b - (math.log(c) + k * a)) ** 2 for a, b in zip(lx, ly))
    return k, c, 1 - ss_res / ss_tot


def klass(k):
    return "flat / O(log n)" if k < 0.15 else ("sub-linear" if k < 0.6 else "O(n)")


def fmt_us(us):
    if us is None or us != us:
        return "n/a"
    if us < 1000:
        return f"{us:.2f} us"
    if us < 1e6:
        return f"{us/1e3:.1f} ms"
    if us < 3.6e9:
        return f"{us/1e6:.1f} s"
    if us < 8.64e10 * 2:
        return f"{us/3.6e9:.1f} h"
    return f"{us/8.64e10:.0f} d"


def fmt_kb(kb):
    b = kb * 1024
    for u, s in (("TB", 1e12), ("GB", 1e9), ("MB", 1e6), ("KB", 1e3)):
        if b >= s:
            return f"{b/s:.1f} {u}"
    return f"{b:.0f} B"


out = []
summ = []
for key in sorted(series):
    grp, comp, op = key
    pts = sorted(series[key].items())
    if len(pts) < 3 or comp == "bloom":
        continue
    xs = [n for n, _ in pts]
    for metric, getter in (("p50_us", lambda r: r["p50_us"]), ("mem_kb", lambda r: r.get("mem_delta_kb") or None)):
        ys = [getter(r) for _, r in pts]
        pairs = [(x, y) for x, y in zip(xs, ys) if y and y > 0]
        if len(pairs) < 3:
            continue
        if metric == "p50_us" and op in ("synth_build", "build", "resident"):
            continue
        fa = fit(*zip(*pairs))
        tail = pairs[-4:]
        ft = fit(*zip(*tail))
        if not fa or not ft:
            continue
        k_all, _, r2 = fa
        k_tail, c_tail, _ = ft
        n_top, y_top = pairs[-1]
        if k_tail < 0.15:
            big = [(x, y) for x, y in pairs if x >= 1e5] or pairs
            lx = [math.log2(x) for x, _ in big]
            n = len(big)
            mx = sum(lx) / n
            my = sum(y for _, y in big) / n
            sxx = sum((a - mx) ** 2 for a in lx) or 1e-9
            b = sum((a - mx) * (y - my) for a, (_, y) in zip(lx, big)) / sxx
            a0 = my - b * mx
            proj = max(a0 + b * math.log2(1e12), y_top)
            model = "a+b*log2(N)"
        else:
            proj = y_top * (1e12 / n_top) ** k_tail
            model = "y_top*(1e12/N_top)^k"
        rec = dict(group=grp, comp=comp, op=op, metric=metric, n_min=xs[0], n_max=n_top, k_all=round(k_all, 3), r2=round(r2, 3),
                   k_tail=round(k_tail, 3), cls=klass(k_tail), y_at_nmax=y_top, projected_1e12=proj, model=model)
        summ.append(rec)

json.dump(summ, open(os.path.join(d, "summary.json"), "w"), indent=1)

lines = ["| component | operation | N range (MEASURED) | exponent k (all) | k (top 4) | class | value at N max (MEASURED) | at 1e12 (PROJECTED) |", "|---|---|---|---|---|---|---|---|"]
for r in summ:
    f = fmt_us if r["metric"] == "p50_us" else fmt_kb
    lines.append(
        f"| {r['comp']} ({r['group']}) | {r['op']}{' memory' if r['metric']=='mem_kb' else ' p50'} | {r['n_min']:.0e} to {r['n_max']:.0e} | {r['k_all']} | {r['k_tail']} | {r['cls']} | {f(r['y_at_nmax'])} | {f(r['projected_1e12'])} |"
    )
open(os.path.join(d, "tables.md"), "w").write("\n".join(lines) + "\n")
print("\n".join(lines))
