#!/usr/bin/env python3
"""Merge a vcs-only re-run (TOOLS=vcs bench_jj_sl.py) into an older vcs/jj/sl result set.

usage: scripts/jj_sl_merge_refresh.py <old-dir with flat/deep/binary.json> <new-dir with vcsonly-<layout>.json> <out-dir>
The jj and sl cells are copied from the old set unchanged; the vcs cells are replaced by the new ones. The merged
meta records both dates. The result is NOT an interleaved run (the vcs and jj/sl samples are from different
sessions); say so wherever it is quoted. Feed <out-dir> to scripts/jj_sl_scoreboard.py.
"""
import json, os, shutil, sys

old, new, out = sys.argv[1:4]
os.makedirs(out, exist_ok=True)
for l in ("flat", "deep", "binary"):
    o = json.load(open(os.path.join(old, l + ".json")))
    n = json.load(open(os.path.join(new, f"vcsonly-{l}.json")))
    keep = [c for c in o["cells"] if c["tool"] != "vcs"]
    vcs = {c["cell"]: c for c in n["cells"] if c["tool"] == "vcs"}
    cells = []
    for c in o["cells"]:
        if c["tool"] == "vcs":
            cells.append(vcs.pop(c["cell"], c))
        else:
            cells.append(c)
    cells += list(vcs.values())
    meta = dict(o["meta"])
    meta["vcs_refresh"] = {"date": n["meta"]["date"], "vcs": n["meta"]["versions"]["vcs"], "load_start": n["meta"]["load_start"]}
    json.dump({"meta": meta, "cells": cells}, open(os.path.join(out, l + ".json"), "w"))
    q = os.path.join(old, "desc-" + l + ".json")
    if os.path.exists(q):
        shutil.copy(q, os.path.join(out, "desc-" + l + ".json"))
