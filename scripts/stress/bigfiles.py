#!/usr/bin/env python3
"""Huge binary files for the monorepo stress run.

  bigfiles.py gen    DIR N        N files bigNN.bin of 1..5 GiB of random bytes (sizes cycle 1, 2, 3, 4, 5 GiB less a bit)
  bigfiles.py mutate DIR VER      next version: 3 x 1 MiB overwritten in every file, 1 MiB inserted in every 5th file,
                                  64 MiB appended to every 4th file (deterministic from VER)
  bigfiles.py hash   DIR OUT.json sha256 + size of every *.bin, in parallel
"""
import hashlib, json, multiprocessing as mp, os, random, sys

MIB = 1 << 20
GIB = 1 << 30


def gen_one(a):
    path, size, seed = a
    rnd = random.Random(seed)
    with open(path, "wb") as f:
        left = size
        while left:
            n = min(64 * MIB, left)
            f.write(os.urandom(n))
            left -= n
    return path


def mutate_one(a):
    path, ver, i = a
    rnd = random.Random(ver * 1000 + i)
    size = os.path.getsize(path)
    if i % 5 == 0:   # insertion shifts everything after it (content-defined chunking must resync)
        off = rnd.randrange(size - 2 * MIB)
        tmp = path + ".tmp"
        with open(path, "rb") as r, open(tmp, "wb") as w:
            left = off
            while left:
                b = r.read(min(64 * MIB, left)); w.write(b); left -= len(b)
            w.write(os.urandom(MIB))
            while True:
                b = r.read(64 * MIB)
                if not b:
                    break
                w.write(b)
        os.replace(tmp, path)
        size = os.path.getsize(path)
    with open(path, "r+b") as f:
        for _ in range(3):
            f.seek(rnd.randrange(size - MIB)); f.write(os.urandom(MIB))
    if i % 4 == 0:
        with open(path, "ab") as f:
            f.write(os.urandom(64 * MIB))
    return path


def hash_one(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        while True:
            b = f.read(16 * MIB)
            if not b:
                break
            h.update(b)
    return os.path.basename(path), h.hexdigest(), os.path.getsize(path)


if __name__ == "__main__":
    cmd, d = sys.argv[1], sys.argv[2]
    names = lambda: sorted(os.path.join(d, f) for f in os.listdir(d) if f.endswith(".bin"))
    with mp.Pool(min(16, os.cpu_count())) as p:
        if cmd == "gen":
            n = int(sys.argv[3])
            os.makedirs(d, exist_ok=True)
            sizes = [(1 + (i % 5)) * GIB - (i % 7) * MIB for i in range(n)]
            list(p.imap_unordered(gen_one, [(os.path.join(d, f"big{i:02d}.bin"), sizes[i], i) for i in range(n)]))
        elif cmd == "mutate":
            ver = int(sys.argv[3])
            list(p.imap_unordered(mutate_one, [(f, ver, i) for i, f in enumerate(names())]))
        elif cmd == "hash":
            res = {n: {"sha256": h, "size": s} for n, h, s in p.imap_unordered(hash_one, names())}
            json.dump(res, open(sys.argv[3], "w"), indent=1, sort_keys=True)
            print(len(res), "files", sum(v["size"] for v in res.values()), "bytes")
