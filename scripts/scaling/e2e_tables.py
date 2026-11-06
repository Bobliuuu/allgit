#!/usr/bin/env python3
"""Markdown tables from the end-to-end rows (e2e*.jsonl, slice.jsonl): one table per (kind, N), vcs next to git.

usage: e2e_tables.py DATA_DIR > tables_e2e.md
"""
import glob, json, os, sys

d = sys.argv[1]
rows = []
for f in sorted(glob.glob(os.path.join(d, "e2e*.jsonl")) + glob.glob(os.path.join(d, "slice*.jsonl"))):
    for l in open(f):
        r = json.loads(l)
        r["src"] = os.path.basename(f)[:-6]
        rows.append(r)

ORDER = ["import", "status_first", "status_second", "log_n20", "rev_list_head", "tag_create", "add_one_file", "commit_one_file",
         "publish_first", "publish_incremental", "clone_full", "gc_first", "gc_second", "fsck", "checkout", "commit_graph_write",
         "add_all", "commit_all", "publish_all", "clone_all", "clone_one_zone", "clone_one_dir"]


def cell(r):
    if r is None:
        return "n/a", "n/a"
    w = r["wall_s"]
    t = f"{w:.2f} s" if w < 100 else f"{w:.0f} s"
    if r.get("rc") not in (0, None):
        t += f" (rc {r['rc']})"
    m = r.get("maxrss_kb")
    return t, (f"{m/1048576:.2f} GiB" if m and m >= 1048576 else (f"{m/1024:.0f} MiB" if m else "n/a"))


groups = {}
for r in rows:
    groups.setdefault((r["src"], r["kind"], r["n"]), []).append(r)
for (src, kind, n), rs in sorted(groups.items(), key=lambda kv: (kv[0][1], kv[0][2], kv[0][0])):
    print(f"\n#### {kind} N={n:,} (dataset `{src}`)\n")
    print("| step | vcs wall | vcs peak RSS | git wall | git peak RSS |\n|---|---|---|---|---|")
    by = {(r["tool"], r["step"]): r for r in rs if "wall_s" in r}
    steps = [s for s in ORDER if ("vcs", s) in by or ("git", s) in by]
    for s in steps:
        v, g = by.get(("vcs", s)), by.get(("git", s))
        vw, vm = cell(v)
        gw, gm = cell(g)
        print(f"| {s} | {vw} | {vm} | {gw} | {gm} |")
    for r in rs:
        if "disk_kb" in r:
            print(f"\n- {r['tool']} {r['step']}: {r['disk_kb']/1048576:.2f} GiB")
        if "files" in r:
            print(f"\n- files in the cloned slice: {r['files']:,}")
