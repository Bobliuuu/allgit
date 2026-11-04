#!/usr/bin/env python3
"""Per-component byte breakdown of a vcs store (.vcs), a git store (.git / bare repo) or a vcs directory remote.

MEASUREMENT ONLY. usage: space_components.py <dir> [<dir>...]   (prints one JSON object per dir, then a table)

Components (first matching rule wins; sizes are apparent bytes = sum of st_size, plus file counts):
  loose        objects/xx/<hex>        loose objects
  pack         *.pack                   pack bodies
  idx          *.idx                    pack indexes
  index        index, index.*           working-tree index / staging cache
  commit-graph commit-graph*, *.graph   commit graph files
  manifest     manifest*, MANIFEST*, refs/heads manifests, *.manifest
  bloom        *bloom*                  changed-path / bloom filters
  other        everything else (reported by top-level name so it can be chased)
"""
import json, os, re, sys

RULES = [
    ("loose", re.compile(r"(^|/)objects/[0-9a-f]{2}/[0-9a-f]+$")),
    ("pack", re.compile(r"\.pack$")),
    ("idx", re.compile(r"\.(idx|rev)$")),
    ("bloom", re.compile(r"bloom", re.I)),
    ("commit-graph", re.compile(r"commit-graph|\.graph$", re.I)),
    ("manifest", re.compile(r"manifest", re.I)),
    ("index", re.compile(r"(^|/)index(\.[^/]*)?$")),
]


def classify(rel):
    for name, rx in RULES:
        if rx.search(rel):
            return name
    return "other"


def measure(root):
    comp, other = {}, {}
    for d, _, fs in os.walk(root):
        for f in fs:
            p = os.path.join(d, f)
            rel = os.path.relpath(p, root)
            st = os.lstat(p)
            c = classify(rel)
            e = comp.setdefault(c, {"bytes": 0, "files": 0, "alloc": 0})
            e["bytes"] += st.st_size
            e["alloc"] += st.st_blocks * 512
            e["files"] += 1
            if c == "other":
                top = "/".join(rel.split("/")[:2])
                other[top] = other.get(top, 0) + st.st_size
    tot = sum(v["bytes"] for v in comp.values())
    return {"dir": root, "total": tot, "components": comp,
            "other_top": dict(sorted(other.items(), key=lambda kv: -kv[1])[:8])}


if __name__ == "__main__":
    rs = [measure(d) for d in sys.argv[1:]]
    for r in rs:
        print(json.dumps(r))
    print()
    for r in rs:
        print(f"{r['dir']}: total {r['total'] / 2**20:.2f} MiB")
        for k, v in sorted(r["components"].items(), key=lambda kv: -kv[1]["bytes"]):
            print(f"  {k:13s} {v['bytes'] / 2**20:9.2f} MiB  {v['files']:6d} files")
        for k, v in r["other_top"].items():
            print(f"    other: {k} {v / 2**20:.2f} MiB")
