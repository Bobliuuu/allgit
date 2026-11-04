#!/usr/bin/env python3
"""Verdicts for vcs vs jj and vs sapling from bench_jj_sl.py output, same rule as command_scoreboard_render.py.
usage: jj_sl_final_summary.py DATA.json OUT.md
WIN: vcs wall and cpu medians <= 0.95x; LOSS: both >= 1.05x; the gap must also exceed MAD(vcs)+MAD(ref) of wall, else TIE;
a wall gap under 2 ms is a TIE. Runs are interleaved so medians of the raw samples are compared."""
import json, statistics as st, sys
D = json.load(open(sys.argv[1]))
mad = lambda a: st.median([abs(x - st.median(a)) for x in a])
cells = {}
for c in D["cells"]:
    cells.setdefault(c["cell"], {})[c["tool"]] = c
tot = {"jj": {"WIN": 0, "TIE": 0, "LOSS": 0}, "sl": {"WIN": 0, "TIE": 0, "LOSS": 0}}
rows, losses = [], []
for name, t in cells.items():
    v = t.get("vcs")
    if not v or not v["wall"]:
        continue
    row = [name, f"{st.median(v['wall']):.1f}"]
    for o in ("jj", "sl"):
        r = t.get(o)
        if not r or not r["wall"] or r.get("fails"):
            row += ["n/a", "NOT-MEASURED" if r else "N/A"]
            continue
        rw = st.median(v["wall"]) / max(st.median(r["wall"]), 0.001)
        rc = st.median(v["cpu"]) / max(st.median(r["cpu"]), 0.001)
        gap = abs(st.median(v["wall"]) - st.median(r["wall"]))
        if gap < 2 or gap <= mad(v["wall"]) + mad(r["wall"]): vd = "TIE"
        elif rw <= .95 and rc <= .95: vd = "WIN"
        elif rw >= 1.05 and rc >= 1.05: vd = "LOSS"
        else: vd = "TIE"
        tot[o][vd] += 1
        if vd == "LOSS": losses.append((name, o, rw, rc, st.median(r["wall"]), st.median(v["wall"])))
        row += [f"{st.median(r['wall']):.1f}", f"{vd} ({rw:.2f}/{rc:.2f})"]
    rows.append(row + [str(v["n"])])
m = D["meta"]
out = ["# vcs vs Jujutsu and Sapling, flat 100k (final run)", "",
       f"Build `{(m.get('build_sha') or '')[:7]}`, {m['versions']['git']}, {m['versions']['jj']}, {m['versions']['sl']}, {m['cpu']}, {m['cores']} vCPU, {m['date']}, runs per cell {m['runs']} (interleaved), "
       "each cell gated on load1 < 1.0. Verdict rule as in command-scoreboard.md (5% band, wall gap above MAD noise, ratios wall/cpu).", "",
       "| tool | WIN | TIE | LOSS |", "|---|---|---|---|"] + [f"| {o} | {tot[o]['WIN']} | {tot[o]['TIE']} | {tot[o]['LOSS']} |" for o in tot] + [
       "", "## Losses", "", "| cell | vs | wall ratio | cpu ratio | ref ms | vcs ms |", "|---|---|---|---|---|---|"] + \
      [f"| {n} | {o} | {a:.2f} | {b:.2f} | {r:.1f} | {v:.1f} |" for n, o, a, b, r, v in losses] + [
       "", "## All cells (median wall ms)", "", "| cell | vcs | jj | verdict vs jj | sl | verdict vs sl | n |", "|---|---|---|---|---|---|---|"] + \
      ["| " + " | ".join([r[0], r[1], r[2], r[3], r[4], r[5], r[6]]) + " |" for r in rows]
open(sys.argv[2], "w").write("\n".join(out) + "\n")
print(tot); print(losses)
