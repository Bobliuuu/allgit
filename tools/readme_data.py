#!/usr/bin/env python3
"""Derive README/video numbers from raw result files already committed under docs/research/data/.

Writes docs/research/data/readme_data.json. Run from the repo root: python3 tools/readme_data.py
Sources:
  docs/research/data/hugescale-boxf/results.tsv   1,000,000-file run, git vs vcs, one run per cell
  docs/research/data/zoneprofile-bench.json       zone-profile clone benchmark, 3 runs per cell
Counts (crates, tests, docs, scripts) are measured from the working tree.
"""
import json
import os
import re
import statistics as st

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
D = os.path.join(ROOT, "docs", "research", "data")
out = {}

# ---- 1M files: git vs vcs
rows = {}
for line in open(os.path.join(D, "hugescale-boxf", "results.tsv")):
    f = line.rstrip("\n").split("\t")
    if len(f) < 8 or f[2] == "du-store":
        continue
    _, tool, op, wall, user, sys_, rss, rc = f[:8]
    rows.setdefault(op, {})[tool] = {"wall": float(wall), "rss_mib": round(int(rss) / 1024), "rc": int(rc)}
assert all(v["rc"] == 0 for r in rows.values() for v in r.values()), "a row failed"
LABEL = {"init": "init", "add": "add all (1M files)", "commit": "commit", "status-clean": "status, clean",
         "status-1pct": "status, 1% modified", "diff": "diff, 1% modified", "checkout-1pct": "checkout previous commit",
         "checkout-back": "checkout back", "publish": "publish (local remote)", "clone": "clone (local path; not like for like)",
         "gc": "gc", "add-noop": "add, nothing changed"}
table = []
for op, lab in LABEL.items():
    g, v = rows[op].get("git"), rows[op].get("vcs")
    r = {"op": lab, "git_s": g["wall"] if g else None, "vcs_s": v["wall"],
         "git_rss": g["rss_mib"] if g else None, "vcs_rss": v["rss_mib"]}
    r["ratio"] = round(g["wall"] / v["wall"], 2) if g else None
    r["verdict"] = None if not g else ("vcs faster" if g["wall"] > v["wall"] else "git faster")
    table.append(r)
out["huge_table"] = table
out["huge_files"] = 1_000_000
cmp_ = [r for r in table if r["git_s"] is not None]
out["huge_vcs_faster"] = sum(r["verdict"] == "vcs faster" for r in cmp_)
out["huge_git_faster"] = sum(r["verdict"] == "git faster" for r in cmp_)
out["huge_cmp_rows"] = len(cmp_)
byop = {r["op"]: r for r in table}
out["huge_add"] = byop["add all (1M files)"]
out["huge_diff"] = byop["diff, 1% modified"]
out["huge_gc"] = byop["gc"]
out["huge_noop"] = byop["add, nothing changed"]
out["huge_clone"] = byop["clone (local path; not like for like)"]
# chart rows: ratio for the ops where vcs wins, plus the losses (ratio < 1) so the chart is honest
out["huge_ratio_rows"] = [{"op": r["op"].replace(" (local path; not like for like)", "*").replace(" (1M files)", ""), "ratio": r["ratio"]}
                          for r in cmp_ if r["op"] not in ("init",)]

# ---- zone profile: how much a profile-chosen clone downloads
z = json.load(open(os.path.join(D, "zoneprofile-bench.json")))
med = lambda k, f: st.median(z[k][f])
out["zone"] = {"zones": z["zones"], "files_per_zone": z["files_per_zone"], "runs": z["runs"], "hot": len(z["hot_zones"]),
               "full_mb": med("clone_full_vcs", "bytes_down") / 1e6, "auto_mb": med("clone_auto_vcs", "bytes_down") / 1e6,
               "full_s": med("clone_full_vcs", "wall_s"), "auto_s": med("clone_auto_vcs", "wall_s"),
               "git_full_s": med("clone_full_git", "wall_s"), "git_sparse_s": med("clone_partial_sparse_git", "wall_s"),
               "full_disk_mb": med("clone_full_vcs", "disk_bytes") / 1e6, "auto_disk_mb": med("clone_auto_vcs", "disk_bytes") / 1e6}
out["zone"]["saved_pct"] = round(100 * (1 - out["zone"]["auto_mb"] / out["zone"]["full_mb"]))
out["zone"]["mb_rows"] = [{"what": "full clone (all zones)", "mb": round(out["zone"]["full_mb"], 1)},
                           {"what": "zones chosen from your profile", "mb": round(out["zone"]["auto_mb"], 1)}]
out["zone"]["s_rows"] = [{"what": "git full clone", "s": round(out["zone"]["git_full_s"], 2)},
                          {"what": "vcs full clone", "s": round(out["zone"]["full_s"], 2)},
                          {"what": "git partial + sparse clone", "s": round(out["zone"]["git_sparse_s"], 2)},
                          {"what": "vcs profile-chosen zones", "s": round(out["zone"]["auto_s"], 2)}]

# ---- repo facts, counted from the tree
crates = sorted(x for x in os.listdir(os.path.join(ROOT, "crates")) if os.path.isfile(os.path.join(ROOT, "crates", x, "Cargo.toml")))
out["crates"] = len(crates)
tests = 0
for dp, _, fs in os.walk(os.path.join(ROOT, "crates")):
    for fn in fs:
        if fn.endswith(".rs"):
            tests += len(re.findall(r"#\[(?:tokio::)?test\]", open(os.path.join(dp, fn), errors="ignore").read()))
out["test_fns"] = tests
out["docs"] = sum(1 for f in os.listdir(os.path.join(ROOT, "docs")) if f.endswith(".md"))
out["research_docs"] = sum(1 for f in os.listdir(os.path.join(ROOT, "docs", "research")) if f.endswith(".md"))
rs = 0
for dp, _, fs in os.walk(os.path.join(ROOT, "crates")):
    rs += sum(len(open(os.path.join(dp, f), errors="ignore").read().splitlines()) for f in fs if f.endswith(".rs"))
out["rust_lines"] = rs
out["version"] = re.search(r'^version = "([^"]+)"', open(os.path.join(ROOT, "Cargo.toml")).read(), re.M).group(1)

with open(os.path.join(D, "readme_data.json"), "w") as f:
    json.dump(out, f, indent=1)
print("wrote docs/research/data/readme_data.json")
