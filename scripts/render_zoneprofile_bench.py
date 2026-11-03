#!/usr/bin/env python3
"""Render result.json of bench_zoneprofile.py as markdown tables (medians and ranges)."""
import json, statistics, sys
r = json.load(open(sys.argv[1]))
def cell(v, f="{:.2f}"):
    v = [x for x in v if x is not None]
    if not v: return "n/a"
    return f"{f.format(statistics.median(v))} [{f.format(min(v))}..{f.format(max(v))}]"
def mb(v):
    v = [x / 1e6 for x in v if x is not None]
    return f"{statistics.median(v):.1f} [{min(v):.1f}..{max(v):.1f}]" if v else "n/a"
def table(title, keys):
    print(f"\n{title}\n\n| scenario | wall s | cpu s | requests | MB downloaded | MB on disk |\n|---|---|---|---|---|---|")
    for name, k in keys:
        d = r[k]
        print(f"| {name} | {cell(d['wall_s'])} | {cell(d['cpu_s'])} | {cell(d['requests'], '{:.0f}')} | {mb(d['bytes_down'])} | {mb(d['disk_bytes'])} |")
table("Initial clone", [("vcs full", "clone_full_vcs"), ("vcs --auto-profile", "clone_auto_vcs"), ("git full", "clone_full_git"), ("git partial (blob:none) + sparse, same zones", "clone_partial_sparse_git")])
table("Incremental sync (1000 files changed in 10 zones)", [("vcs sync", "sync_full_vcs"), ("vcs sync --auto", "sync_auto_vcs"), ("git pull, full clone", "sync_full_git"), ("git pull, partial + sparse", "sync_partial_sparse_git")])
print("\nOn-demand fetch of one skipped zone:", json.dumps(r["on_demand_vcs"]), "git sparse-checkout add:", json.dumps(r["on_demand_git"]))
print("\nstatus with/without recording (ms):", json.dumps(r["status_wall_ms"]))
print("\nhot zones:", r["hot_zones"], "| load1 start/end:", r["load1_start"], r["load1_end"], "|", r["git_version"], "|", r["vcs_version"], "| cores", r["cores"])
