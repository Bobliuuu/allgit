#!/usr/bin/env python3
"""Render walgit_full.py results (res-*.json timing, cnt-*.json counting) as Markdown: per-dataset tables and a
scoreboard (walgit vs vcs default vs vcs best) with an explicit list of cells where walgit wins.
usage: report_full.py <dir with res-*.json and cnt-*.json> > tables.md"""
import glob, json, os, sys

D = sys.argv[1]
ARMS = ["git", "walgit", "vcs_def", "vcs_best", "vcs_fast"]
VARMS = ["vcs_def", "vcs_best", "vcs_fast"]
LAB = {"git": "git (local bare)", "walgit": "walgit", "vcs_def": "vcs default", "vcs_best": "vcs best-documented (pack v3)", "vcs_fast": "vcs fastest-config (pack v1, raw)"}
OPS = [("init_push", "initial push"), ("inc_push", "1-file push"),
       ("clone_full_warm", "full clone, warm"), ("clone_full_cold", "full clone, cold"),
       ("clone_partial_warm", "partial/zone clone, warm"), ("clone_partial_cold", "partial/zone clone, cold"),
       ("fetch_1_warm", "fetch 1 commit, warm"), ("fetch_1_cold", "fetch 1 commit, cold"),
       ("fetch_noop_warm", "no-op fetch, warm"), ("fetch_noop_cold", "no-op fetch, cold")]
POST = [("post_" + k, "after maint: " + n) for k, n in OPS if k not in ("init_push", "inc_push")]
TIE = 0.05


def med(c):
    return c["median"] if c else None


def fmt_ms(c):
    return "-" if not c else f"{c['median']:.0f} ({c['min']:.0f}-{c['max']:.0f})"


def mib(kib):
    return "-" if kib is None else f"{kib/1024:.1f}"


def cpu(r):
    """client + front-server + RustFS CPU seconds (sum of the three medians)."""
    if not r:
        return None
    t = 0.0
    for k in ("client_cpu_s", "srv_cpu_s", "rf_cpu_s"):
        if r.get(k):
            t += r[k]["median"]
    return t


def rss(r):
    return None if not r or not r.get("srv_rss_mib") else r["srv_rss_mib"]["median"]


def load(pattern):
    out = {}
    for f in sorted(glob.glob(os.path.join(D, pattern))):
        d = json.load(open(f))
        out[(d["meta"]["layout"], d["meta"]["n_files"])] = d
    return out


R = load("res-*.json")
C = load("cnt-*.json")
score = []  # (dataset, metric, kind, walgit, [vcs_def, vcs_best, vcs_fast])


def add(ds, name, kind, w, vs):
    if w is not None and all(v is not None for v in vs):
        score.append((ds, name, kind, w, vs))



def keyset(k):
    return sorted(R, key=lambda x: (x[1], x[0]))


def ratio_txt(w, v):
    if w is None or v is None or w == 0 and v == 0:
        return "n/a"
    if w == 0:
        return "inf"
    return f"{v/w:.2f}x"


for key in keyset(R):
    d = R[key]; m = d["meta"]; res = d["res"]
    ds = f"{m['layout']} {m['n_files']}"
    print(f"\n### {ds} files ({m['bytes']/1048576:.1f} MiB, g0 = {m['g0_files']} files), reps {m['reps']}, init reps {m['init_reps']}, load1 start {m['load_start'][0]}, end {m.get('load_end', ['?'])[0]}\n")
    print("Wall ms, median (min-max). CPU = client + front-server + RustFS CPU seconds (sum of medians). srv RSS = peak VmHWM of walgit-server / vcs-server in MiB.\n")
    print("| op | " + " | ".join(LAB[a] for a in ARMS if a in res) + " |")
    print("|---|" + "---|" * len([a for a in ARMS if a in res]))
    for k, name in OPS + POST:
        cells = []
        for a in ARMS:
            if a not in res:
                continue
            r = res[a].get(k)
            if not r:
                cells.append("n/a"); continue
            c = cpu(r); s = rss(r)
            cells.append(f"{fmt_ms(r['wall_ms'])}; cpu {c:.2f}s" + (f"; rss {s:.0f}" if s else ""))
            if a in ("walgit", "vcs_def", "vcs_best"):
                pass
        print(f"| {name} | " + " | ".join(cells) + " |")
        w = res.get("walgit", {}).get(k)
        vs = [res.get(a, {}).get(k) for a in VARMS]
        if w and all(vs):
            add(ds, name + " wall ms", "speed", med(w["wall_ms"]), [med(v["wall_ms"]) for v in vs])
            add(ds, name + " total CPU s", "speed", cpu(w), [cpu(v) for v in vs])
            if rss(w) and all(rss(v) for v in vs):
                add(ds, name + " server RSS MiB", "memory", rss(w), [rss(v) for v in vs])
    # sizes
    print("\nSizes (MiB; du = allocated blocks, app = apparent bytes). Side effect only: speed is the primary goal.\n")
    print("| size | " + " | ".join(LAB[a] for a in ARMS if a in res) + " |")
    print("|---|" + "---|" * len([a for a in ARMS if a in res]))

    def srow(name, f):
        cells = []
        for a in ARMS:
            if a not in res:
                continue
            try:
                cells.append(f(res[a]))
            except Exception:
                cells.append("n/a")
        print(f"| {name} | " + " | ".join(cells) + " |")

    srow("remote store after initial push (du / app)", lambda r: f"{mib(r['store_after_init']['du_kib'])} / {mib(r['store_after_init']['apparent_kib'])}")
    srow("remote store after maintenance (du / app)", lambda r: f"{mib(r['maint']['remote_after']['du_kib'])} / {mib(r['maint']['remote_after']['apparent_kib'])}")
    srow("full clone: client store (.git / .vcs)", lambda r: f"{mib(r['clone_full_facts']['client_store_kib'])}")
    srow("partial clone: files / client store", lambda r: f"{r['clone_partial_facts']['checked_out_files']} / {mib(r['clone_partial_facts']['client_store_kib'])}")
    for a in ARMS:
        pass
    # maintenance detail
    print("\nMaintenance (time ms / total CPU s / result size MiB):\n")
    print("| arm | remote maintenance | client maintenance variants |")
    print("|---|---|---|")
    for a in ARMS:
        if a not in res:
            continue
        mt = res[a].get("maint", {})
        rem = []
        for k in ("gc_default", "gc_aggressive", "compact", "ops_compact_force_base"):
            if mt.get(k) and "wall_ms" in mt[k]:
                extra = ""
                if k == "gc_aggressive":
                    extra = f" -> {mib(mt.get('gc_aggressive_size_kib'))}"
                rem.append(f"{k}: {mt[k]['wall_ms']:.0f} ms / {sum(mt[k].get(x) or 0 for x in ('client_cpu_s','srv_cpu_s','rf_cpu_s')):.1f} s{extra}")
        if mt.get("remote_after_settle"):
            rem.append(f"after maintainer settle: {mib(mt['remote_after_settle']['du_kib'])}")
        if mt.get("remote_before"):
            rem.append(f"before: {mib(mt['remote_before']['du_kib'])}, after: {mib(mt.get('remote_after', {}).get('du_kib'))}")
        cl = [f"before {mib(mt.get('client_before_kib'))}"]
        for k, v in mt.get("client", {}).items():
            if "wall_ms" in v:
                cl.append(f"{k}: {v['wall_ms']:.0f} ms / {mib(v['size_kib'])}" + ("" if v.get("fsck_ok", True) else " FSCK FAIL"))
            else:
                cl.append(f"{k}: error")
        print(f"| {LAB[a]} | " + "; ".join(rem) + " | " + "; ".join(cl) + " |")
    # counting
    if key in C:
        c = C[key]["res"]
        print("\nObject-store requests / payload bytes (counting proxy, 1 run per op; GET+PUT+other, MiB up/down):\n")
        ca = [a for a in ("walgit",) + tuple(VARMS) if a in c]
        print("| op | " + " | ".join(LAB[a] for a in ca) + " |")
        print("|---|" + "---|" * len(ca))
        for k, name in [("init_push", "initial push"), ("inc_push", "1-file push")] + OPS[2:] + POST:
            cells = []
            for a in ca:
                v = c[a].get(k + "_proxy")
                if not v:
                    cells.append("n/a"); continue
                bm = v["by_method"]; oth = v["requests"] - bm.get("GET", 0) - bm.get("PUT", 0)
                cells.append(f"{v['requests']} ({bm.get('GET',0)}/{bm.get('PUT',0)}/{oth}) {v['bytes_up']/1048576:.1f} / {v['bytes_down']/1048576:.1f}")
            print(f"| {name} | " + " | ".join(cells) + " |")
            vs = [c[a].get(k + "_proxy") for a in ("walgit",) + tuple(VARMS)]
            if all(vs):
                add(ds, name + " store requests", "speed", vs[0]["requests"], [v["requests"] for v in vs[1:]])
                tb = lambda v: (v["bytes_up"] + v["bytes_down"]) / 1048576
                add(ds, name + " store traffic up+down MiB", "traffic", tb(vs[0]), [tb(v) for v in vs[1:]])
        if c.get("walgit", {}).get("idle_65s"):
            v = c["walgit"]["idle_65s"]
            print(f"\nwalgit idle (maintainer loop, one repo, 65 s): {v['requests']} store requests; vcs-server idle: 0 by construction (stateless; not separately counted).")
    # size scoreboard rows
    try:
        w = res["walgit"]
        g = lambda r, f: f(r)
        fs = [("remote store after initial push MiB (du)", lambda r: r["store_after_init"]["du_kib"] / 1024),
              ("remote store after maintenance MiB (du)", lambda r: r["maint"]["remote_after"]["du_kib"] / 1024),
              ("full-clone client store MiB (du)", lambda r: r["clone_full_facts"]["client_store_kib"] / 1024),
              ("partial-clone client store MiB (du)", lambda r: r["clone_partial_facts"]["client_store_kib"] / 1024)]
        for nm, f in fs:
            add(ds, nm, "space", f(w), [f(res[a]) for a in VARMS])
    except Exception:
        pass

LABS = {"vcs_def": "default", "vcs_best": "best-doc", "vcs_fast": "fastest"}


def res_(w, v):
    if w is None or v is None:
        return "n/a"
    if w == 0 and v == 0:
        return "tie"
    if w == 0:
        return "LOSS"
    r = v / w
    return "win" if r < 1 - TIE else "LOSS" if r > 1 + TIE else "tie"


def f(x):
    return "-" if x is None else f"{x:.2f}" if x < 100 else f"{x:.0f}"


def board(kinds, title):
    rows = [x for x in score if x[2] in kinds]
    print(f"\n## {title}\n")
    print("Lower is better. Ratio = vcs / walgit (below 1.00x: vcs better). `win` = vcs lower by more than 5%, `tie` = within 5%, `LOSS` = walgit lower by more than 5%. Medians only.\n")
    print("| dataset | metric | walgit | " + " | ".join(f"vcs {LABS[a]} | ratio | result" for a in VARMS) + " |")
    print("|---|---|---|" + "---|---|---|" * len(VARMS))
    tally = {a: [0, 0, 0] for a in VARMS}
    losses = []
    for ds, name, kind, w, vs in rows:
        cells = []
        lost = []
        for a, v in zip(VARMS, vs):
            r = res_(w, v)
            if r in ("win", "tie", "LOSS"):
                tally[a][["win", "tie", "LOSS"].index(r)] += 1
            cells.append(f"{f(v)} | {ratio_txt(w, v)} | {r}")
            if r == "LOSS":
                lost.append(a)
        print(f"| {ds} | {name} | {f(w)} | " + " | ".join(cells) + " |")
        if lost:
            losses.append((ds, name, w, vs, lost))
    print(f"\nTotals over {len(rows)} cells (win / tie / LOSS): " + "; ".join(f"vcs {LABS[a]} {tally[a]}" for a in VARMS) + ".\n")
    print(f"### Cells where walgit still beats vcs ({title.lower()})\n")
    print("| dataset | metric | walgit | " + " | ".join(f"vcs {LABS[a]}" for a in VARMS) + " | vcs variants that lose |")
    print("|---|---|---|" + "---|" * len(VARMS) + "---|")
    for ds, name, w, vs, lost in losses:
        print(f"| {ds} | {name} | {f(w)} | " + " | ".join(f(v) for v in vs) + " | " + ", ".join(LABS[a] for a in lost) + " |")


board(("speed",), "Scoreboard 1: speed (wall, CPU, store requests)")
board(("memory", "traffic"), "Scoreboard 2: server memory and object-store traffic")
board(("space",), "Scoreboard 3: space (secondary)")
