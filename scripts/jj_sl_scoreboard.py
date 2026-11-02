#!/usr/bin/env python3
"""Turn bench_jj_sl.py JSON results into per-command tables (markdown) and colour-coded SVG scoreboards.

usage: scripts/jj_sl_scoreboard.py <results-dir with flat.json deep.json binary.json> <img-dir> <tables.md>
Ratio = other tool's median wall ms / vcs median wall ms (>1: vcs faster). TIE threshold: within 10% (0.91 to 1.10).
WIN: ratio >= 1.10; LOSS: ratio <= 1/1.10; TIE otherwise. N/A: vcs or the other tool has no equivalent command, or the cell failed.
"""
import json, os, statistics, sys

RES, IMG, MD = sys.argv[1], sys.argv[2], sys.argv[3]
NOTE = sys.argv[4] if len(sys.argv) > 4 else "Box c (16 vCPU, shared), 100k files, interleaved runs."
TIE = 1.10
LAYOUTS = ["flat", "deep", "binary"]
NAME = {"jj": "Jujutsu (jj)", "sl": "Sapling (sl)"}
data = {}
for l in LAYOUTS:
    p = os.path.join(RES, l + ".json")
    if os.path.exists(p): data[l] = json.load(open(p))
    q = os.path.join(RES, "desc-" + l + ".json")   # optional re-run of single cells (CELLS=describe) replaces those cells
    if l in data and os.path.exists(q):
        new = json.load(open(q))["cells"]; names = {c["cell"] for c in new}
        out, done = [], False
        for c in data[l]["cells"]:
            if c["cell"] in names:
                if not done: out += new; done = True
            else: out.append(c)
        data[l]["cells"] = out

def med(c, k): return statistics.median(c[k])

def cells(layout):
    out = {}
    for c in data[layout]["cells"]:
        out.setdefault(c["cell"], {})[c["tool"]] = c
    return out

def verdict(r):
    if r >= TIE: return "WIN"
    if r <= 1 / TIE: return "LOSS"
    return "TIE"

def fmt(ms): return f"{ms:,.0f}"

order = []
for l in data:
    for c in data[l]["cells"]:
        if c["cell"] not in order: order.append(c["cell"])

def row(layout, cellname, other):
    d = cells(layout).get(cellname)
    if not d: return None
    o = d.get(other); v = d.get("vcs")
    if o is None or o["fails"] == o["n"]: return dict(status="N/A", why=f"no {other} result" if o is None else f"{other} command failed", o=o, v=v)
    if v is None: return dict(status="N/A", why="vcs has no equivalent command", o=o, v=None)
    if v["fails"] == v["n"]: return dict(status="N/A", why="vcs command failed", o=o, v=v)
    r = med(o, "wall") / med(v, "wall")
    return dict(status=verdict(r), ratio=r, o=o, v=v)

md = []
losses = {}
for other in ("jj", "sl"):
    md.append(f"### vcs vs {NAME[other]}\n")
    md.append(f"| command | layout | {other} wall ms (cpu) | vcs wall ms (cpu) | ratio {other}/vcs | result | runs |")
    md.append("|---|---|---|---|---|---|---|")
    for cellname in order:
        for l in data:
            r = row(l, cellname, other)
            if r is None: continue
            o, v = r["o"], r["v"]
            os_ = f"{fmt(med(o,'wall'))} ({fmt(med(o,'cpu'))})" if o else "N/A"
            vs_ = f"{fmt(med(v,'wall'))} ({fmt(med(v,'cpu'))})" if v else "N/A"
            if r["status"] == "N/A":
                md.append(f"| {cellname} | {l} | {os_} | {vs_} | N/A ({r['why']}) | N/A | {o['n'] if o else '-'}/{v['n'] if v else '-'} |")
            else:
                md.append(f"| {cellname} | {l} | {os_} | {vs_} | {r['ratio']:.2f} | {r['status']} | {o['n']}/{v['n']} |")
                if r["status"] == "LOSS": losses.setdefault(other, []).append((cellname, l, r["ratio"], med(o, "wall"), med(v, "wall")))
    md.append("")
    wins = sum(1 for c in order for l in data if (row(l, c, other) or {}).get("status") == "WIN")
    ties = sum(1 for c in order for l in data if (row(l, c, other) or {}).get("status") == "TIE")
    ls = len(losses.get(other, []))
    md.append(f"Totals vs {other}: {wins} WIN, {ties} TIE, {ls} LOSS.\n")
    md.append(f"LOSSES vs {other} (vcs slower by more than 10%):\n")
    if ls:
        md.append("| command | layout | vcs ms | " + other + " ms | vcs slower by |")
        md.append("|---|---|---|---|---|")
        for (c, l, r, om, vm) in losses[other]:
            md.append(f"| {c} | {l} | {fmt(vm)} | {fmt(om)} | {1/r:.2f}x |")
    else: md.append("none")
    md.append("")
open(MD, "w").write("\n".join(md))

COL = {"WIN": ("--win", "WIN"), "TIE": ("--tie", "TIE"), "LOSS": ("--loss", "LOSS"), "N/A": ("--na", "N/A")}
def svg(other):
    cw, rh, lw, top = 150, 30, 270, 92
    W = lw + cw * len(data) + 20; H = top + rh * len(order) + 52
    s = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {W} {H}" width="{W}" height="{H}" font-family="system-ui,sans-serif" font-size="13" role="img" aria-label="vcs vs {NAME[other]} scoreboard">',
         "<style>:root{--bg:#fff;--fg:#1b1f24;--grid:#d0d7de;--win:#1a7f37;--tie:#9a6700;--loss:#cf222e;--na:#8c959f;--on:#fff}"
         "@media (prefers-color-scheme:dark){:root{--bg:#0d1117;--fg:#e6edf3;--grid:#30363d;--win:#2ea043;--tie:#bf8700;--loss:#f85149;--na:#6e7681;--on:#0d1117}}"
         ".bg{fill:var(--bg)}.t{fill:var(--fg)}.h{font-weight:600}.sm{font-size:11px;opacity:.8}.c{fill:var(--on);text-anchor:middle;font-weight:600}</style>",
         f'<rect class="bg" width="{W}" height="{H}"/>',
         f'<text class="t h" x="12" y="26" font-size="17">allgit vcs vs {NAME[other]}: per-command scoreboard</text>',
         f'<text class="t sm" x="12" y="46">cell = {other} median wall / vcs median wall (ratio; above 1 means vcs is faster). WIN &gt;= 1.10, LOSS &lt;= 0.91, else TIE.</text>',
         f'<text class="t sm" x="12" y="62">N/A = no equivalent command or failed. {NOTE}</text>']
    for j, l in enumerate(data):
        s.append(f'<text class="t h" x="{lw + cw*j + cw/2}" y="{top-8}" text-anchor="middle">{l}</text>')
    for i, c in enumerate(order):
        y = top + rh * i
        s.append(f'<text class="t" x="12" y="{y+rh/2+4}">{c}</text>')
        for j, l in enumerate(data):
            r = row(l, c, other)
            x = lw + cw * j
            if r is None: st, label = "N/A", "N/A"
            else:
                st = r["status"]; label = "N/A" if st == "N/A" else f'{r["ratio"]:.2f}x {st}'
            var = COL[st][0]
            s.append(f'<rect x="{x+3}" y="{y+3}" width="{cw-6}" height="{rh-6}" rx="4" fill="var({var})"/>')
            s.append(f'<text class="c" x="{x+cw/2}" y="{y+rh/2+4}">{label}</text>')
    cnt = {k: 0 for k in COL}
    for c in order:
        for l in data:
            r = row(l, c, other); cnt[r["status"] if r else "N/A"] += 1
    s.append(f'<text class="t h" x="12" y="{H-18}">Totals: {cnt["WIN"]} WIN, {cnt["TIE"]} TIE, {cnt["LOSS"]} LOSS, {cnt["N/A"]} N/A</text>')
    s.append("</svg>")
    return "\n".join(s)

os.makedirs(IMG, exist_ok=True)
for other, f in (("jj", "scoreboard-jj.svg"), ("sl", "scoreboard-sapling.svg")):
    open(os.path.join(IMG, f), "w").write(svg(other))
