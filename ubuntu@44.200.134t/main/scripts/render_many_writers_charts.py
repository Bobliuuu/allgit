#!/usr/bin/env python3
"""Charts for docs/research/many-writers-s3.md from docs/research/many-writers-data/*.jsonl.
  python3 scripts/render_many_writers_charts.py   -> docs/img/charts/many-writers-*.{svg,png}"""
import json, os
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
D = os.path.join(ROOT, "docs/research/many-writers-data")
OUT = os.path.join(ROOT, "docs/img/charts")
os.makedirs(OUT, exist_ok=True)


def load(name):
    return [json.loads(l) for l in open(os.path.join(D, name))]


def series(rows, mode, key):
    r = sorted((x for x in rows if x["mode"] == mode), key=lambda x: x["N"])
    return [x["N"] for x in r], [x[key] for x in r]


THEMES = {"light": dict(bg="#fcfcfb", fg="#0b0b0b", mut="#52514e", grid="#e3e2de", a="#e34948", b="#2a78d6"),
          "dark": dict(bg="#1a1a19", fg="#f0efec", mut="#c3c2b7", grid="#383835", a="#e66767", b="#3987e5")}
s3o, s3n = load("s3-before-disjoint.jsonl"), load("s3-batch-disjoint.jsonl")
dko, dkn = load("disk-before.jsonl"), load("disk-batch.jsonl")

for tn, t in THEMES.items():
    plt.rcParams.update({"figure.facecolor": t["bg"], "axes.facecolor": t["bg"], "savefig.facecolor": t["bg"], "text.color": t["fg"],
                         "axes.labelcolor": t["mut"], "xtick.color": t["mut"], "ytick.color": t["mut"], "axes.edgecolor": t["grid"],
                         "font.size": 10, "axes.spines.top": False, "axes.spines.right": False})
    fig, ax = plt.subplots(1, 3, figsize=(13, 3.8))
    for a in ax:
        a.grid(True, color=t["grid"], lw=.6)
        a.set_xscale("log", base=2)
        a.set_xlabel("simulated writers (threads)")
    for lab, rows, col in (("one manifest PUT per push (before)", s3o, t["a"]), ("batched manifest commit (after)", s3n, t["b"])):
        x, y = series(rows, "zones", "pps"); ax[0].plot(x, y, "o-", color=col, label=lab)
        x, y = series(rows, "zones", "p99"); ax[1].plot(x, [v / 1000 for v in y], "o-", color=col, label=lab)
    for lab, rows, col in (("before", dko, t["a"]), ("after", dkn, t["b"])):
        x, y = series(rows, "zones", "pps"); ax[2].plot(x, y, "o-", color=col, label=lab)
    ax[0].set_title("Real S3 us-east-1: pushes/s, disjoint zones", loc="left", fontsize=10)
    ax[1].set_title("Real S3: p99 push latency (s)", loc="left", fontsize=10)
    ax[2].set_title("Local disk (fsync): pushes/s, disjoint zones", loc="left", fontsize=10)
    ax[0].set_ylabel("pushes per second"); ax[1].set_ylabel("seconds"); ax[2].set_ylabel("pushes per second")
    ax[0].legend(frameon=False, fontsize=8)
    fig.text(0.01, 0.01, "c6i.4xlarge, vcs-server + 4 driver processes on one box. Data: docs/research/many-writers-data/. 2-3 pushes per writer, one run per cell.",
             fontsize=7, color=t["mut"])
    fig.tight_layout(rect=(0, 0.04, 1, 1))
    for ext in ("svg", "png"):
        fig.savefig(os.path.join(OUT, f"many-writers-scaling-{tn}.{ext}"), dpi=140)
    plt.close(fig)

    # same-branch chart
    fig, ax = plt.subplots(figsize=(6.2, 3.8))
    ax.grid(True, color=t["grid"], lw=.6)
    pts = {"disk, before": [(r["N"], r["pps"]) for r in dko if r["mode"] == "same"],
           "disk, retry jitter + batch": [(r["N"], r["pps"]) for r in dkn if r["mode"] == "same"],
           "S3, before": [(r["N"], r["pps"]) for r in load("s3-same-old.jsonl")],
           "S3, batch + jitter": [(r["N"], r["pps"]) for r in load("s3-batch-same.jsonl")]}
    for (lab, p), col in zip(pts.items(), (t["a"], t["b"], t["mut"], t["fg"])):
        ax.plot([a for a, b in p], [b for a, b in p], "o-", color=col, label=lab)
    ax.set_xscale("log", base=2); ax.set_yscale("symlog", linthresh=1)
    ax.set_xlabel("writers on ONE branch"); ax.set_ylabel("acked pushes/s (0 = collapsed within 240 s)")
    ax.set_title("One hot branch: retry storm, not CAS fan-in", loc="left", fontsize=10)
    ax.legend(frameon=False, fontsize=8)
    fig.tight_layout()
    for ext in ("svg", "png"):
        fig.savefig(os.path.join(OUT, f"many-writers-samebranch-{tn}.{ext}"), dpi=140)
    plt.close(fig)
print("ok")
