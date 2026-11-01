#!/usr/bin/env python3
"""Deterministic synthetic history as a `git fast-import` stream (stdout).

The same stream feeds `git fast-import` and `vcs import`, so both tools measure the same history.

usage: histgen.py [--commits 20000] [--dirs 20] [--subdirs 20] [--files 50] [--seed 1]
                  [--branch-prob 0.08] [--tag-every 1000]

Shape: a tree of dirs*subdirs directories with `files` files each (default 20000 files, 3 levels),
every file about 40 lines. Each commit edits 1-3 files (zipf-skewed, so a few files are hot), a few
commits add, rename or delete a file. About `branch-prob` of the commits start a short side branch
(1-8 commits) that is merged back with a merge commit. Time grows by 1-600 s per commit. An
annotated tag `v<k>` is made every `tag-every` first-parent commits. The path `hot/app.txt`
(about 400 lines) is edited often and is the --follow / blame / -S target; `hot/old_name.txt` is
renamed to `hot/app.txt` early in history.
"""
import argparse, random, sys

ap = argparse.ArgumentParser()
ap.add_argument("--commits", type=int, default=20000)
ap.add_argument("--dirs", type=int, default=20)
ap.add_argument("--subdirs", type=int, default=20)
ap.add_argument("--files", type=int, default=50)
ap.add_argument("--seed", type=int, default=1)
ap.add_argument("--branch-prob", type=float, default=0.08)
ap.add_argument("--tag-every", type=int, default=1000)
a = ap.parse_args()
rnd = random.Random(a.seed)
out = sys.stdout.buffer
W = out.write

def data(b):
    W(b"data %d\n" % len(b)); W(b); W(b"\n")

files = {}  # path -> list of lines
paths = []
for d in range(a.dirs):
    for s in range(a.subdirs):
        for f in range(a.files):
            p = "d%02d/s%02d/f%03d.txt" % (d, s, f)
            files[p] = ["p%d_%d_%d line %d v0" % (d, s, f, i) for i in range(40)]
            paths.append(p)
HOT = "hot/app.txt"
OLD = "hot/old_name.txt"
files[OLD] = ["hot line %d v0" % i for i in range(400)]

mark = 0
t = 1_600_000_000
tip = None  # mark of main tip
msgn = 0

def body(lines):
    return ("\n".join(lines) + "\n").encode()

def commit(ref, ts, msg, parents, changes, merge=None):
    global mark
    mark += 1
    m = mark
    W(b"commit %s\nmark :%d\n" % (ref.encode(), m))
    W(b"author A U Thor <a@x.invalid> %d +0000\ncommitter A U Thor <a@x.invalid> %d +0000\n" % (ts, ts))
    data(msg.encode())
    if parents:
        W(b"from :%d\n" % parents[0])
        for p in parents[1:]:
            W(b"merge :%d\n" % p)
    for ch in changes:
        if ch[0] == "M":
            W(b"M 100644 inline %s\n" % ch[1].encode()); data(body(ch[2]))
        elif ch[0] == "D":
            W(b"D %s\n" % ch[1].encode())
        elif ch[0] == "R":
            W(b"R %s %s\n" % (ch[1].encode(), ch[2].encode()))
    return m

def zipf_pick():
    # skewed: 30% of edits go to the first 1% of files
    if rnd.random() < 0.3:
        return paths[rnd.randrange(max(1, len(paths) // 100))]
    return paths[rnd.randrange(len(paths))]

def edit(p):
    ls = files[p]
    for _ in range(rnd.randint(1, 3)):
        i = rnd.randrange(len(ls))
        ls[i] = "%s e%d" % (ls[i].split(" e")[0], rnd.randrange(10**6))
    if rnd.random() < 0.2:
        ls.insert(rnd.randrange(len(ls)), "inserted %d" % rnd.randrange(10**6))
    return ("M", p, list(ls))

def make_changes(allow_struct=True):
    ch = []
    r = rnd.random()
    if r < 0.05:
        ch.append(edit(HOT if HOT in files else OLD))
    for _ in range(rnd.randint(1, 3)):
        ch.append(edit(zipf_pick()))
    if allow_struct and rnd.random() < 0.01 and len(paths) > 100:
        i = rnd.randrange(len(paths)); p = paths[i]
        if rnd.random() < 0.5:
            q = "d%02d/s%02d/g%06d.txt" % (rnd.randrange(a.dirs), rnd.randrange(a.subdirs), rnd.randrange(10**6))
            if q not in files:
                files[q] = files.pop(p); paths[i] = q
                ch = [c for c in ch if c[1] != p]
                ch.append(("R", p, q))
        else:
            q = "d%02d/s%02d/n%06d.txt" % (rnd.randrange(a.dirs), rnd.randrange(a.subdirs), rnd.randrange(10**6))
            files[q] = ["new file %s line %d" % (q, i) for i in range(30)]; paths.append(q)
            ch.append(("M", q, files[q]))
    # dedupe: last write per path wins
    seen = {}
    for c in ch:
        seen[(c[1] if c[0] != "R" else c[2])] = c
    return list(seen.values())

# initial commit: everything
init = [("M", p, ls) for p, ls in files.items()]
t += 1
tip = commit("refs/heads/main", t, "initial import", [], init)

n = 1
first_parent = 1
while n < a.commits:
    t += rnd.randint(1, 600)
    if n >= 50 and OLD in files:
        # rename the hot file (content kept, slightly edited)
        files[HOT] = files.pop(OLD)
        files[HOT][5] = "hot line 5 renamed"
        tip = commit("refs/heads/main", t, "rename hot file", [tip], [("R", OLD, HOT), ("M", HOT, files[HOT])])
        n += 1; first_parent += 1
        continue
    if rnd.random() < a.branch_prob and n + 10 < a.commits:
        # side branch: edits on a snapshot-free model (same file model; merge is trivially clean
        # because side edits are applied to the shared model in order, so the merged tree equals the model)
        k = rnd.randint(1, 8)
        side = tip
        touched = {}
        for j in range(k):
            t += rnd.randint(1, 60)
            chs = make_changes(False)
            for c in chs:
                touched[c[1]] = c
            side = commit("refs/heads/side", t, "side %d" % n, [side], chs)
            n += 1
        t += rnd.randint(1, 60)
        # fast-import does not merge trees: the merge commit re-states what the side branch changed
        tip = commit("refs/heads/main", t, "merge side %d" % n, [tip, side], list(touched.values()))
        n += 1; first_parent += 1
    else:
        tip = commit("refs/heads/main", t, "change %d" % n, [tip], make_changes())
        n += 1; first_parent += 1
    if first_parent % a.tag_every == 0:
        W(b"tag v%d\nfrom :%d\ntagger A U Thor <a@x.invalid> %d +0000\n" % (first_parent // a.tag_every, tip, t))
        data(b"release")
W(b"done\n")
