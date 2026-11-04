#!/usr/bin/env python3
"""Render result tables (markdown) from a raw JSONL written by harness.py. Usage: report.py live.jsonl"""
import collections
import json
import statistics as st
import sys

rows = [json.loads(l) for l in open(sys.argv[1])]
live = "tokens_in" in rows[0]


def agg(rs):
    n = len(rs)
    if not n:
        nan = float('nan')
        return dict(n=0, acc=nan, calls=nan, bytes=nan, wall=nan, tin=nan, cost=0, llm=nan)
    d = {"n": n, "acc": sum(r["correct"] for r in rs) / n, "calls": st.mean(r["tool_calls"] for r in rs),
         "bytes": st.mean(r["out_bytes"] for r in rs), "wall": st.mean(r["wall_secs"] for r in rs)}
    if live:
        d["tin"] = st.mean(r["tokens_in"] for r in rs)
        d["cost"] = sum(r["cost_usd"] for r in rs)
        d["llm"] = st.mean(r["llm_secs"] for r in rs)
    return d


def line(label, g, v):
    cells = [label]
    for k, f in [("n", "{}"), ("acc", "{:.0%}"), ("calls", "{:.1f}"), ("bytes", "{:,.0f}"), ("wall", "{:.1f}")] + ([("tin", "{:,.0f}")] if live else []):
        cells.append(f.format(g[k]) + " / " + f.format(v[k]) if k != "n" else f.format(g[k]))
    return "| " + " | ".join(cells) + " |"


hdr = "| | episodes/tool | correct (git / vcs) | tool calls | tool output bytes | wall s |" + (" prompt tokens |" if live else "")
sep = "|---|---|---|---|---|---|" + ("---|" if live else "")
print("Cells are git / vcs.\n")
print(hdr)
print(sep)
by = lambda tool, f=lambda r: True: [r for r in rows if r["tool"] == tool and f(r)]
print(line("**all tasks**", agg(by("git")), agg(by("vcs"))))
for c in sorted({r["category"] for r in rows}):
    print(line(c, agg(by("git", lambda r: r["category"] == c)), agg(by("vcs", lambda r: r["category"] == c))))
print("\nPer task (git / vcs):\n")
print("| task | category | correct (of repeats) | mean calls | mean bytes |")
print("|---|---|---|---|---|")
order = list(dict.fromkeys(r["task"] for r in rows))
for t in order:
    g, v = by("git", lambda r: r["task"] == t), by("vcs", lambda r: r["task"] == t)
    if not g or not v:
        continue
    print(f"| {t} | {g[0]['category']} | {sum(r['correct'] for r in g)}/{len(g)} / {sum(r['correct'] for r in v)}/{len(v)} | "
          f"{st.mean(r['tool_calls'] for r in g):.1f} / {st.mean(r['tool_calls'] for r in v):.1f} | "
          f"{st.mean(r['out_bytes'] for r in g):,.0f} / {st.mean(r['out_bytes'] for r in v):,.0f} |")
if live:
    print(f"\nTotal API cost (list price estimate): ${sum(r['cost_usd'] for r in rows):.3f}; "
          f"mean LLM seconds per episode git/vcs: {agg(by('git'))['llm']:.1f} / {agg(by('vcs'))['llm']:.1f}")
