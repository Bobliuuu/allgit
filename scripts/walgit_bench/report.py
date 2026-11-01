#!/usr/bin/env python3
"""Render res-<layout>-<n>.json files from walgit_bench.py as Markdown tables.
usage: report.py data/walgit-bench/res-*.json > tables.md"""
import json, sys


def m(c, f="{:.0f}"):
    if not c:
        return "-"
    return f.format(c["median"]) + f" ({f.format(c['min'])}-{f.format(c['max'])})"


def kb(x):
    return "-" if x is None else (f"{x/1048576:.1f} MiB" if x >= 1048576 else f"{x/1024:.0f} KiB")


for path in sys.argv[1:]:
    d = json.load(open(path))
    mt = d["meta"]
    print(f"\n### {mt['layout']} {mt['n_files']} files ({mt['bytes']/1048576:.1f} MiB, g0 = {mt['g0_files']} files), reps {mt['reps']}, init reps {mt['init_reps']}, load1 at start {mt['load_start'][0]}, end {mt.get('load_end', ['?'])[0]}\n")
    print("Wall ms, median (min-max). srv = front-server CPU seconds (walgit-server incl. reaped git children / vcs-server), rf = RustFS CPU seconds.\n")
    print("| op | variant | git (local bare) | walgit | vcs-server |")
    print("|---|---|---|---|---|")
    T = d["timing"]

    def row(op, var, getter):
        cells = []
        for t in ("git", "walgit", "vcs"):
            try:
                o = getter(T[t])
                w = m(o["wall_ms"])
                extra = ""
                if o.get("srv_cpu_s"):
                    extra += f" srv {o['srv_cpu_s']['median']:.2f}s"
                if t != "git" and o.get("rustfs_cpu_s"):
                    extra += f" rf {o['rustfs_cpu_s']['median']:.2f}s"
                cells.append(w + extra)
            except (KeyError, TypeError):
                cells.append("n/a")
        print(f"| {op} | {var} | " + " | ".join(cells) + " |")

    row("initial push", "51 commits", lambda t: t["init_push"])
    row("1-file push", "", lambda t: t["inc_push"])
    for var in ("warm", "cold_os", "cold_server"):
        row("full clone", var, lambda t, v=var: t["clone_full"][v])
    for var in ("warm", "cold_os", "cold_server"):
        row("partial clone (10%)", var, lambda t, v=var: t["clone_partial"][v])
    for var in ("warm", "cold_os", "cold_server"):
        row("fetch 1 commit", var, lambda t, v=var: t["fetch_1"][v])
    print()
    print("| fact | git | walgit | vcs |")
    print("|---|---|---|---|")
    def fact(name, f):
        out = []
        for t in ("git", "walgit", "vcs"):
            try:
                out.append(str(f(T[t])))
            except Exception:
                out.append("n/a")
        print(f"| {name} | " + " | ".join(out) + " |")
    K = mt["init_reps"]
    fact(f"remote store per repo after initial push (du; walgit/vcs bucket du / {K} identical repos)", lambda t: kb(t["store_after_init_kib"] * 1024 / (1 if "wall_ms" in t["init_push"] and t is T["git"] else K)))
    fact("full clone: files checked out / client store", lambda t: f"{t['clone_full']['checked_out_files']} / {kb(t['clone_full']['client_store_kib']*1024)}")
    fact("partial clone: files checked out / client store", lambda t: f"{t['clone_partial']['checked_out_files']} / {kb(t['clone_partial']['client_store_kib']*1024)}")
    print("\nObject-store requests / payload bytes (counting proxy, 1 run per op; up = to store, down = from store):\n")
    C = d["counting"]
    ops = []
    for t in C:
        for k in C[t]:
            if k not in ops:
                ops.append(k)
    print("| op | walgit req (GET/PUT/other) up / down | vcs-server req (GET/PUT/other) up / down |")
    print("|---|---|---|")
    for k in ops:
        cells = []
        for t in ("walgit", "vcs"):
            v = C.get(t, {}).get(k)
            if not v:
                cells.append("n/a")
                continue
            bm = v["by_method"]
            oth = v["requests"] - bm.get("GET", 0) - bm.get("PUT", 0)
            cells.append(f"{v['requests']} ({bm.get('GET',0)}/{bm.get('PUT',0)}/{oth}) {kb(v['bytes_up'])} / {kb(v['bytes_down'])}")
        print(f"| {k} | " + " | ".join(cells) + " |")
