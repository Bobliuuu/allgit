#!/usr/bin/env python3
"""Scaling charts (same style as scripts/render_showcase_charts.py) -> docs/img/charts/scaling-*-{light,dark}.{svg,png}.

  python3 scripts/scaling/render_charts.py

Reads docs/research/data/scaling/*.jsonl (MEASURED points, solid markers). Dashed lines past the last marker are the PROJECTED
fit from analyze.py's model and are drawn only on the "to 1e12" chart, in a lighter tone and labelled.
"""
import glob, json, math, os, textwrap
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DATA = os.path.join(ROOT, "docs/research/data/scaling")
OUT = os.path.join(ROOT, "docs/img/charts")
os.makedirs(OUT, exist_ok=True)

THEMES = {
    "light": dict(bg="#fcfcfb", fg="#0b0b0b", mut="#52514e", grid="#e3e2de", c=["#2a78d6", "#eb6834", "#1baf7a", "#6250d6", "#c98500", "#e34948"]),
    "dark": dict(bg="#1a1a19", fg="#f0efec", mut="#c3c2b7", grid="#383835", c=["#3987e5", "#d95926", "#199e70", "#9085e9", "#eda100", "#e66767"]),
}


def setup(t):
    plt.rcParams.update({
        "figure.facecolor": t["bg"], "axes.facecolor": t["bg"], "savefig.facecolor": t["bg"],
        "text.color": t["fg"], "axes.labelcolor": t["mut"], "xtick.color": t["mut"], "ytick.color": t["mut"],
        "axes.edgecolor": t["grid"], "font.family": "DejaVu Sans", "font.size": 10, "svg.fonttype": "path",
    })


def style(ax, t):
    for s in ("top", "right"):
        ax.spines[s].set_visible(False)
    ax.spines["left"].set_color(t["grid"]); ax.spines["bottom"].set_color(t["grid"])
    ax.grid(color=t["grid"], lw=0.8); ax.set_axisbelow(True)


def title(fig, t, head, sub, foot):
    fig.text(0.015, 0.975, head, fontsize=14, fontweight="bold", va="top", color=t["fg"])
    fig.text(0.015, 0.925, sub, fontsize=9.5, va="top", color=t["mut"])
    fig.text(0.015, 0.012, "\n".join(textwrap.wrap(foot, 150)), fontsize=8, va="bottom", color=t["mut"], linespacing=1.3)


def save(fig, name, mode):
    for ext in ("svg", "png"):
        fig.savefig(os.path.join(OUT, f"scaling-{name}-{mode}.{ext}"), dpi=150)
    plt.close(fig)


def load():
    rows = {}
    for f in glob.glob(os.path.join(DATA, "*.jsonl")):
        b = os.path.basename(f)
        if b.startswith("e2e"):
            continue
        for l in open(f):
            r = json.loads(l)
            if "comp" in r:
                rows[(b[:-6], r["comp"], r["op"], r["n"])] = r
    return rows


def series(rows, grp, comp, op, key="p50_us"):
    pts = sorted((k[3], v[key]) for k, v in rows.items() if k[0] == grp and k[1] == comp and k[2] == op and v.get(key))
    return [p[0] for p in pts], [p[1] for p in pts]


def lines(mode, t, rows, name, head, sub, specs, ylabel, foot, scale=1.0, project=False):
    fig, ax = plt.subplots(figsize=(9, 6))
    fig.subplots_adjust(left=0.1, right=0.78, top=0.85, bottom=0.2)
    style(ax, t)
    for i, (label, grp, comp, op, key) in enumerate(specs):
        xs, ys = series(rows, grp, comp, op, key)
        if not xs:
            continue
        ys = [y * scale for y in ys]
        col = t["c"][i % len(t["c"])]
        ax.plot(xs, ys, marker="o", ms=4, lw=1.8, color=col)
        ax.text(xs[-1] * 1.25, ys[-1], label, color=col, fontsize=8.5, va="center")
        if project and len(xs) >= 3:
            lx = [math.log(x) for x in xs[-4:]]; ly = [math.log(y) for y in ys[-4:]]
            m = len(lx); mx = sum(lx) / m; my = sum(ly) / m
            k = sum((a - mx) * (b - my) for a, b in zip(lx, ly)) / (sum((a - mx) ** 2 for a in lx) or 1)
            k = max(k, 0.0)
            ax.plot([xs[-1], 1e12], [ys[-1], ys[-1] * (1e12 / xs[-1]) ** k], ls="--", lw=1.2, color=col, alpha=0.55)
    ax.set_xscale("log"); ax.set_yscale("log")
    ax.set_xlabel("N (objects / commits / files / entries)"); ax.set_ylabel(ylabel)
    title(fig, t, head, sub, foot)
    save(fig, name, mode)


def render(mode, t):
    rows = load()
    foot_m = "MEASURED on one i4i.8xlarge (32 vCPU, 256 GB, NVMe), synthetic structures, real reader code, p50 of 20,000 ops unless noted. Source: docs/research/data/scaling/*.jsonl."
    lines(mode, t, rows, "lookup", "Lookup latency vs N (MEASURED)", "Point lookups in id-sorted structures, log-log, 1e3 to 1e9 entries. A flat line is O(1)/O(log n).",
          [("pack index hit", "pack_index", "pack_index", "lookup_hit", "p50_us"), ("pack index miss", "pack_index", "pack_index", "lookup_miss_random", "p50_us"),
           ("pack index, 256-bit near collision", "pack_index", "pack_index", "lookup_miss_near_collision", "p50_us"),
           ("reach bitmap member", "reach", "reach_bitmap", "member_lookup", "p50_us"), ("AGI5 index find", "agi5", "agi5", "find_hit", "p50_us"),
           ("commit-graph index_of", "commit_graph", "commit_graph", "index_of", "p50_us")], "microseconds", foot_m)
    lines(mode, t, rows, "wholefile", "Whole-structure operations vs N (MEASURED)", "Operations that read the entire structure: linear by construction, and the memory they need.",
          [("commit-graph decode", "commit_graph", "commit_graph", "decode_validate", "p50_us"), ("reach cache load+verify", "reach", "reach_bitmap", "load_verify", "p50_us"),
           ("path-filter file load", "pfilter", "path_filter", "load_parse", "p50_us"), ("AGI5 verify_all", "agi5", "agi5", "verify_all", "p50_us"),
           ("pack index verify_full (fsck)", "pack_index", "pack_index", "verify_full", "p50_us"), ("AGI5 status scan", "agi5", "agi5", "scan_all_status_pass", "p50_us")],
          "microseconds", foot_m)
    lines(mode, t, rows, "manifest", "Manifest and lock table vs N (MEASURED)", "One CAS commit on the single manifest decodes and re-encodes everything; a sharded commit touches one zone.",
          [("single manifest CAS (disk)", "manifest", "manifest", "single_commit_cas_fs", "p50_us"), ("sharded commit, one zone", "manifest", "manifest", "sharded_commit_one_zone", "p50_us"),
           ("sharded snapshot (all zones)", "manifest", "manifest", "sharded_snapshot_all_zones", "p50_us"),
           ("lock acquire (apply)", "locks", "locks", "acquire_validate_apply", "p50_us")], "microseconds", foot_m)
    lines(mode, t, rows, "projection", "To 1e12: the same lines, extrapolated (PROJECTED dashes)", "Solid markers are MEASURED; dashes are the power-law fit of the last four points carried to 1e12, nothing more.",
          [("pack index hit", "pack_index", "pack_index", "lookup_hit", "p50_us"), ("reach member", "reach", "reach_bitmap", "member_lookup", "p50_us"),
           ("commit-graph decode", "commit_graph", "commit_graph", "decode_validate", "p50_us"), ("single manifest CAS", "manifest", "manifest", "single_commit_cas_fs", "p50_us"),
           ("sharded commit", "manifest", "manifest", "sharded_commit_one_zone", "p50_us")], "microseconds",
          "MEASURED markers as in the other charts; PROJECTED dashes are an extrapolation of a model, not a measurement. Model and caveats: docs/research/scaling-to-trillions.md.", project=True)


for mode, t in THEMES.items():
    setup(t)
    render(mode, t)
print("ok")
