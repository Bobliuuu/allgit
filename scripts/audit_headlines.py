#!/usr/bin/env python3
"""Recompute the README headline numbers from the committed raw data and flag disagreements.

  python3 -I scripts/audit_headlines.py            # from the repository root; reads only docs/research/**, no network, no build

Cheap and safe to run anywhere (reads a few JSON/TSV files, a few ms). It does NOT re-time anything: re-timing needs the
remote boxes (see docs/research/benchmark-audit.md, "Needs a remote run"). Exit status 1 if any check disagrees.
"""
import json, os, statistics as st, sys

R = "docs/research"
bad = []


def med(x):
    return st.median(x)


def check(name, got, want, tol=0.005):
    ok = abs(got - want) <= tol * max(1.0, abs(want))
    print(("ok   " if ok else "FAIL ") + f"{name}: data {got:.4g}, doc {want:.4g}")
    if not ok:
        bad.append(name)


# 1M files (huge-scale): results.tsv wall column
rows = {}
for l in open(f"{R}/data/hugescale-boxf/results.tsv"):
    f = l.rstrip("\n").split("\t")
    rows[(f[1], f[2])] = (float(f[3]), float(f[4]), float(f[5]))  # wall, user, sys
for (t, op, want) in [("git", "clone", 603.1), ("vcs", "clone", 40.5), ("git", "add", 215.5), ("vcs", "add", 18.9),
                      ("git", "diff", 19.7), ("vcs", "diff", 3.7), ("git", "add-noop", 0.9), ("vcs", "add-noop", 6.8)]:
    check(f"huge-scale {t} {op} wall", rows[(t, op)][0], want, 0.01)
for t, op in [("git", "clone"), ("git", "add"), ("git", "checkout-1pct"), ("git", "diff")]:
    w, u, s = rows[(t, op)]
    print(f"info  huge-scale {t} {op}: wall/CPU = {w / (u + s):.1f}  (>>1 means I/O wait, not CPU)")

# command scoreboard (100k flat): medians of the committed JSON
for grp, cell, tool, idx, want in [("fresh", "add_all", "git", 0, 5383), ("fresh", "add_all", "vcs", 0, 733),
                                   ("fresh", "commit_all", "git", 0, 414), ("fresh", "commit_all", "vcs", 0, 240),
                                   ("clean", "status", "git", 0, 100.2), ("clean", "status", "vcs", 0, 30.8),
                                   ("clean", "log5", "git", 0, 2.2), ("clean", "log5", "vcs", 0, 2.9)]:
    d = json.load(open(f"{R}/data/command-scoreboard/git-{grp}.json"))
    r = d["cells"][cell]["rows"][tool]
    check(f"scoreboard {cell} {tool} wall ms (n={len(r)}, build {d['meta']['tools']['build_sha'][:7]})", med([x[0] for x in r]), want, 0.05 if want < 10 else 0.01)

# many writers on real S3
def pps(fn, mode, i):
    rows = [json.loads(l) for l in open(f"{R}/many-writers-data/{fn}")]
    return [r for r in rows if r["mode"] == mode][i]["pps"]

lo = min(pps("s3-batch-disjoint.jsonl", m, i) for m in ("zones", "branches") for i in (1, 2, 3))
hi = max(pps("s3-batch-disjoint.jsonl", m, i) for m in ("zones", "branches") for i in (1, 2, 3))
print(f"info  many-writers batch commit, 256 to 1024 writers: {lo} to {hi} pushes/s (README now says 157 to 348)")
check("many-writers min pushes/s", lo, 157.5, 0.01)
check("many-writers max pushes/s", hi, 347.6, 0.01)

# walgit remote workflows (README table)
d = json.load(open(f"{R}/data/walgit-full-boxc/res-flat-100000.json"))["res"]
for arm, key, want in [("git", "init_push", 6554), ("walgit", "init_push", 8398), ("vcs_def", "init_push", 788),
                       ("vcs_def", "clone_full_warm", 2174), ("vcs_def", "clone_full_cold", 1241)]:
    check(f"walgit-table {arm} {key}", med(d[arm][key]["wall_ms"]["runs"]), want, 0.01)
for arm in ("git", "walgit", "vcs_def"):
    w, c = med(d[arm]["clone_full_warm"]["wall_ms"]["runs"]), med(d[arm]["clone_full_cold"]["wall_ms"]["runs"])
    print(f"info  {arm}: full clone 'warm' {w:.0f} ms vs 'cold' {c:.0f} ms" + ("  <- warm slower than cold, label needs checking" if w > c else ""))

# real S3
for fn, want_cold, want_warm in [("std100k", 341, 264), ("exp100k", 245, 149)]:
    d = json.load(open(f"{R}/data/real-s3/{fn}.json"))
    check(f"real-s3 {fn} cold slice clone", med(d["s1_initial"]["vcs --for (slice) [S3, cold: no client cache]"]["wall_ms"]), want_cold, 0.01)
    check(f"real-s3 {fn} warm fetch", med(d["s2"]["vcs --for (slice) [S3, warm client cache]"]["wall_ms"]), want_warm, 0.01)
    after = med(d["s1_after_pushes"]["vcs --for (slice) [S3, cold: no client cache]"]["wall_ms"])
    gitafter = med(d["s1_after_pushes"]["git blobless sparse [git://, same box]"]["wall_ms"])
    print(f"info  real-s3 {fn} cold slice clone AFTER the push series: vcs {after:.0f} ms vs git blobless {gitafter:.0f} ms")

# soak / chaos
v = json.load(open(f"{R}/box-harvest-data/soak-chaos/verify.json"))
kills = sum(1 for l in open(f"{R}/box-harvest-data/soak-chaos/chaos.log") if " kill -9 " in l)
check("soak kills", kills, 157, 0)
check("soak acked files", v["acked_files"], 245, 0)
check("soak acked missing", v["acked_missing"], 0, 0)

print("\nFAILED: " + ", ".join(bad) if bad else "\nall checked headline numbers agree with the raw data")
sys.exit(1 if bad else 0)
