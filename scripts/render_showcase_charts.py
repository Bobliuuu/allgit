#!/usr/bin/env python3
"""Render the README showcase charts to docs/img/charts/*-{light,dark}.{svg,png}.

  python3 scripts/render_showcase_charts.py

Every number is copied from a committed document or data file named in the chart footer. Nothing is estimated.
Inputs read from disk: docs/research/data/hugescale-boxf/results.tsv, docs/research/box-harvest-data/soak-chaos/samples.csv,
docs/research/box-harvest-data/soak-chaos/chaos.log.
"""
import csv, os
import matplotlib
matplotlib.use("Agg")
import matplotlib.pyplot as plt

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "docs/img/charts")
os.makedirs(OUT, exist_ok=True)

THEMES = {
    "light": dict(bg="#fcfcfb", fg="#0b0b0b", mut="#52514e", grid="#e3e2de",
                  vcs="#2a78d6", git="#eb6834", walgit="#1baf7a", p4="#6250d6", good="#008300", warn="#c98500", bad="#e34948"),
    "dark": dict(bg="#1a1a19", fg="#f0efec", mut="#c3c2b7", grid="#383835",
                 vcs="#3987e5", git="#d95926", walgit="#199e70", p4="#9085e9", good="#2fa84f", warn="#eda100", bad="#e66767"),
}


def setup(t):
    plt.rcParams.update({
        "figure.facecolor": t["bg"], "axes.facecolor": t["bg"], "savefig.facecolor": t["bg"],
        "text.color": t["fg"], "axes.labelcolor": t["mut"], "xtick.color": t["mut"], "ytick.color": t["mut"],
        "axes.edgecolor": t["grid"], "font.family": "DejaVu Sans", "font.size": 10,
        "svg.fonttype": "path",
    })


def style(ax, t, grid="x"):
    for s in ("top", "right"):
        ax.spines[s].set_visible(False)
    ax.spines["left"].set_color(t["grid"]); ax.spines["bottom"].set_color(t["grid"])
    ax.grid(axis=grid, color=t["grid"], lw=0.8); ax.set_axisbelow(True)


def title(fig, t, head, sub, foot):
    fig.text(0.015, 0.975, head, fontsize=14, fontweight="bold", va="top", color=t["fg"])
    fig.text(0.015, 0.925, sub, fontsize=9.5, va="top", color=t["mut"])
    import textwrap
    fig.text(0.015, 0.012, "\n".join(textwrap.wrap(foot, 150)), fontsize=8, va="bottom", color=t["mut"], linespacing=1.3)


def save(fig, name, mode):
    for ext in ("svg", "png"):
        fig.savefig(os.path.join(OUT, f"{name}-{mode}.{ext}"), dpi=150)
    plt.close(fig)


def hugescale(mode, t):
    rows = {}
    with open(os.path.join(ROOT, "docs/research/data/hugescale-boxf/results.tsv")) as f:
        for r in csv.reader(f, delimiter="\t"):
            rows[(r[1], r[2])] = float(r[3])
    ops = [("add", "add all"), ("commit", "commit"), ("status-clean", "status, clean"), ("status-1pct", "status, 1% modified"),
           ("diff", "diff, 1% modified"), ("checkout-1pct", "checkout prev"), ("checkout-back", "checkout back"),
           ("clone", "clone"), ("gc", "gc"), ("add-noop", "add, nothing changed")]
    fig, ax = plt.subplots(figsize=(9, 6.2))
    fig.subplots_adjust(left=0.2, right=0.93, top=0.84, bottom=0.12)
    style(ax, t)
    n = len(ops); h = 0.36
    for i, (k, lab) in enumerate(ops):
        y = n - 1 - i
        g, v = rows[("git", k)], rows[("vcs", k)]
        ax.barh(y + h / 2 + 0.02, g, h, color=t["git"], label="git" if i == 0 else None)
        ax.barh(y - h / 2 - 0.02, v, h, color=t["vcs"], label="vcs" if i == 0 else None)
        ax.text(g * 1.08, y + h / 2 + 0.02, f"{g:g} s", va="center", fontsize=8.5, color=t["mut"])
        verdict = "" if v < g * 0.9 else ("  (tie)" if v < g * 1.1 else "  (git faster)")
        ax.text(v * 1.08, y - h / 2 - 0.02, f"{v:g} s{verdict}", va="center", fontsize=8.5, color=t["fg"], fontweight="bold" if not verdict else "normal")
    ax.set_yticks(range(n)); ax.set_yticklabels([l for _, l in ops][::-1])
    ax.set_xscale("log"); ax.set_xlim(0.3, 2500)
    ax.set_xticks([0.5, 1, 10, 100, 1000]); ax.set_xticklabels(["0.5 s", "1 s", "10 s", "100 s", "1000 s"])
    ax.legend(frameon=False, loc="lower right", labelcolor=t["fg"])
    title(fig, t, "1,000,000 files: vcs vs git", "Wall seconds, log scale, shorter is better. One run per cell on a c6i.4xlarge (16 vCPU).",
          "Source: docs/research/huge-scale.md, data/hugescale-boxf/results.tsv. 'nested' layout only; vcs uses 1.0 to 2.3 GiB peak RSS on add/publish/clone.")
    save(fig, "huge-scale", mode)


def groupcommit(mode, t):
    fig, axs = plt.subplots(1, 3, figsize=(10, 4.4))
    fig.subplots_adjust(left=0.07, right=0.98, top=0.76, bottom=0.2, wspace=0.35)
    spec = [("Throughput (publishes/s)", [[42.2, 57.3], [27.3, 69.7]], "{:g}"),
            ("p95 latency (s)", [[3.4, 1.4], [37.2, 8.5]], "{:g}"),
            ("CAS rounds lost (%)", [[87.6, 0.0], [96.9, 0.0]], "{:g}")]
    for ax, (lab, vals, fmt) in zip(axs, spec):
        style(ax, t, "y")
        for j, (grp, off_on) in enumerate(zip(["64 writers", "512 writers"], vals)):
            if lab.startswith("Throughput") or lab.startswith("p95") or lab.startswith("CAS"):
                pass
        # vals[0] = 64 writers (off, on) ; vals[1] = 512 writers (off, on)
        x = [0, 1, 2.6, 3.6]
        data = [vals[0][0], vals[0][1], vals[1][0], vals[1][1]]
        cols = [t["git"], t["vcs"], t["git"], t["vcs"]]
        # 'off' is orange-ish only to mean the baseline, 'on' is vcs blue
        ax.bar(x, data, 0.9, color=cols)
        for xi, d in zip(x, data):
            ax.text(xi, d + max(data) * 0.02, fmt.format(d), ha="center", fontsize=8.5, color=t["fg"])
        ax.set_xticks([0.5, 3.1]); ax.set_xticklabels(["64 writers", "512 writers"])
        ax.set_title(lab, fontsize=10, color=t["fg"], loc="left")
    # the 512-writer lost-% for 'on' is "about 0"; 64 'on' is 0
    axs[0].bar([0], [0], color=t["git"], label="group commit off")
    axs[0].bar([0], [0], color=t["vcs"], label="group commit on")
    h, l = axs[0].get_legend_handles_labels()
    fig.legend(h, l, frameon=False, loc="upper right", bbox_to_anchor=(0.98, 0.9), ncol=2, fontsize=9, labelcolor=t["fg"])
    title(fig, t, "Group commit in vcs-server (writers contending on one manifest)",
          "Through the server over HTTP, medians of 3 runs, box E load1 17 to 22. 512 writers: 5.3 vs 67.8 requests per publish.",
          "Source: docs/scalability.md finding 6 (#452). Direct-to-store publishers (no server) are unchanged; 'on' lost rate for 512 writers is 'about 0'.")
    save(fig, "group-commit", mode)


def highload(mode, t):
    fig, ax = plt.subplots(figsize=(8.5, 5))
    fig.subplots_adjust(left=0.09, right=0.72, top=0.82, bottom=0.2)
    style(ax, t, "y")
    xs = [8, 32, 128]
    series = [("git (daemon, disk)", [112.8, 289.5, 222.0], t["git"], "-"),
              ("vcs (server, disk)", [66.2, 99.7, 85.3], t["vcs"], "-"),
              ("vcs (server, RustFS S3)", [44.8, 43.9, 42.2], t["vcs"], "--"),
              ("walgit (RustFS S3)", [42.0, 37.5, 15.5], t["walgit"], "--")]
    for lab, ys, c, ls in series:
        ax.plot(range(3), ys, ls, color=c, lw=2, marker="o", ms=6, mfc=t["bg"], mew=2)
        ax.text(2.06, ys[-1] + {"vcs (server, RustFS S3)": 9, "walgit (RustFS S3)": -9}.get(lab, 0), f"{lab}: {ys[-1]:g}", va="center", fontsize=8.5, color=t["fg"])
    ax.set_xticks(range(3)); ax.set_xticklabels([f"{x} pushers" for x in xs]); ax.set_xlim(-0.15, 2.0)
    ax.set_ylim(0, 320); ax.set_ylabel("pushes per second (higher is better)")
    title(fig, t, "Many pushers, separate branches: git wins on disk, vcs wins on S3",
          "vcs on S3 stays flat as pushers grow; walgit falls from 42 to 15.5. git on local disk delivers more pushes/s than vcs.",
          "Source: docs/research/highload-matrix.md (with group commit #452), box D, median of 3. git was not run against S3. Every push verified by a fresh clone.")
    save(fig, "highload", mode)


def scoreboard(mode, t):
    data = [("git (flat 100k)", 64, 59, 8), ("Perforce", 17, 7, 6), ("walgit (remote)", 24, 0, 0),
            ("Jujutsu (flat)", 16, 1, 0), ("Sapling (flat)", 15, 1, 0)]
    fig, ax = plt.subplots(figsize=(9, 4))
    fig.subplots_adjust(left=0.18, right=0.93, top=0.78, bottom=0.22)
    ax.set_xlim(0, 150)
    style(ax, t)
    n = len(data)
    for i, (lab, w, ti, l) in enumerate(data):
        y = n - 1 - i
        left = 0
        for v, c in ((w, t["good"]), (ti, t["warn"]), (l, t["bad"])):
            if v:
                ax.barh(y, v - 0.5, 0.6, left=left, color=c)
                if v >= 4:
                    ax.text(left + v / 2 - 0.25, y, str(v), ha="center", va="center", fontsize=9, color="#ffffff", fontweight="bold")
                left += v
        ax.text(left + 1, y, f"{w} / {ti} / {l}", va="center", fontsize=8.5, color=t["mut"])
    ax.set_yticks(range(n)); ax.set_yticklabels([d[0] for d in data][::-1])
    ax.set_xlabel("measured cells")
    ax.bar([0], [0], color=t["good"], label="vcs win"); ax.bar([0], [0], color=t["warn"], label="tie")
    ax.bar([0], [0], color=t["bad"], label="vcs loss")
    ax.legend(frameon=False, loc="lower right", ncol=1, labelcolor=t["fg"], fontsize=8.5)
    title(fig, t, "Per-command scoreboards: wins, ties and losses",
          "Win = at least 10% less wall and CPU; loss = 10% more in both; tie = anything else or under 2 ms. Not-measured cells excluded.",
          "Source: README scoreboard table, docs/research/command-scoreboard.md, jj-sapling-benchmark.md. Single pass, build 845610a, box c.")
    save(fig, "scoreboard-summary", mode)


def zoneprofile(mode, t):
    fig, axs = plt.subplots(1, 2, figsize=(9.5, 4.2))
    fig.subplots_adjust(left=0.2, right=0.97, top=0.76, bottom=0.22, wspace=0.55)
    labs = ["vcs --auto\n(7 zones + root)", "vcs full\n(50 zones)", "git partial +\nsparse (same zones)", "git full"]
    cols = [t["vcs"], t["vcs"], t["git"], t["git"]]
    for ax, vals, unit, ttl, note in (
            (axs[0], [37.0, 189.5, 39.6, 197.4], "MB", "Downloaded / on disk (MB)", True),
            (axs[1], [1.16, 2.87, 3.68, 7.80], "s", "Clone wall time (s)", False)):
        style(ax, t)
        for i, (v, c) in enumerate(zip(vals, cols)):
            ax.barh(3 - i, v, 0.6, color=c, alpha=1.0 if i in (0, 2) else 0.55)
            ax.text(v + max(vals) * 0.02, 3 - i, f"{v:g} {unit}", va="center", fontsize=8.5, color=t["fg"])
        ax.set_yticks(range(4)); ax.set_yticklabels(labs[::-1] if ax is axs[0] else [""] * 4, fontsize=8.5)
        ax.set_xlim(0, max(vals) * 1.25); ax.set_title(ttl, fontsize=10, loc="left", color=t["fg"])
    title(fig, t, "Automatic zone selection: take what you work on",
          "100,051 files, 50 zones, seeded persona profile. vcs counts wire MB; git counts .git growth (different measures, treat as a tie on bytes).",
          "Source: docs/research/zone-profile-bench.md, data/zoneprofile-bench.json. Loopback, 3 runs. --auto makes 71 requests vs 28 for a full clone; git pull beats vcs --auto on incremental sync.")
    save(fig, "zone-profile", mode)


def peering(mode, t):
    fig, ax = plt.subplots(figsize=(8.5, 4.6))
    fig.subplots_adjust(left=0.1, right=0.97, top=0.8, bottom=0.2)
    style(ax, t, "y")
    ns = [64, 128, 256]
    direct = [(146 + 129) / 2, 257, 513]
    cold = [(84 + 96) / 2, (165 + 170) / 2, (384 + 378) / 2]
    warm = [3, 3, 3]
    w = 0.26
    for k, (lab, vals, c) in enumerate((("clients straight to origin", direct, t["git"]),
                                         ("cold read-through peer", cold, t["walgit"]),
                                         ("warm peer, manifest ttl 30 s", warm, t["vcs"]))):
        xs = [i + (k - 1) * w for i in range(3)]
        ax.bar(xs, vals, w - 0.03, color=c, label=lab)
        for x, v in zip(xs, vals):
            ax.text(x, v + 8, f"{v:g}", ha="center", fontsize=8.5, color=t["fg"])
    ax.set_xticks(range(3)); ax.set_xticklabels([f"{n} concurrent clones" for n in ns])
    ax.set_ylabel("requests reaching the origin")
    ax.legend(frameon=False, loc="upper left", labelcolor=t["fg"], fontsize=9)
    title(fig, t, "Read-through peers absorb read load",
          "Origin requests for a batch of N clones (mean of 2 reps). Clone latency is a tie at every N; this measures load, not WAN speed.",
          "Source: docs/peering.md load test, box C, loopback, 9.8 MiB repo. Bytes saved over a real wide-area link were not measured.")
    save(fig, "peering", mode)


def soak(mode, t):
    base = os.path.join(ROOT, "docs/research/box-harvest-data/soak-chaos")
    ts, packs, man = [], [], []
    with open(os.path.join(base, "samples.csv")) as f:
        for r in csv.DictReader(f):
            ts.append(float(r["t"])); packs.append(float(r["packs"])); man.append(float(r["manifest_bytes"]) / 1024)
    kills = []
    t0 = None
    with open(os.path.join(base, "chaos.log")) as f:
        for line in f:
            p = line.split()
            if t0 is None:
                t0 = float(p[0])
            if "kill" in line:
                kills.append(float(p[0]) - t0)
    fig, ax = plt.subplots(figsize=(9, 4.2))
    fig.subplots_adjust(left=0.08, right=0.92, top=0.8, bottom=0.22)
    style(ax, t, "y")
    for k in kills:
        ax.axvline(k, color=t["bad"], lw=0.5, alpha=0.35)
    ax.plot(ts, packs, color=t["vcs"], lw=2)
    ax.set_xlabel("seconds since first kill"); ax.set_ylabel("packs on the remote")
    ax.text(ts[-1], packs[-1], f"  {int(packs[-1])} packs", color=t["fg"], fontsize=8.5, va="center")
    ax.text(0.99, 0.35, f"red lines: {len(kills)} kill -9 of vcs-server", transform=ax.transAxes, color=t["mut"], fontsize=9, va="top", ha="right")
    ax.text(0.99, 0.27, "245 acknowledged files: 0 missing, 0 wrong content, fresh clone fsck ok", transform=ax.transAxes, color=t["fg"], fontsize=9, va="top", fontweight="bold", ha="right")
    title(fig, t, "Chaos: kill -9 the server while writers push",
          "About 14 minutes on one box. Every acknowledged file was looked up in a fresh clone afterwards.",
          "Source: docs/research/box-harvest.md, box-harvest-data/soak-chaos/. Short run: no memory or fd trend under kills, no kill during gc.")
    save(fig, "soak-chaos", mode)


for mode, t in THEMES.items():
    setup(t)
    for fn in (hugescale, groupcommit, highload, scoreboard, zoneprofile, peering, soak):
        fn(mode, t)
print("ok")
