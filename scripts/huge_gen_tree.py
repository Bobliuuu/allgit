#!/usr/bin/env python3
"""Generate a large working tree. usage: huge_gen_tree.py <dest> <layout> <n>
layouts: flat1k (dirs of 1000 files), nested (100x100xN/10000 files per leaf), wide (all n in one dir), deep (chain of depth n, 5 files per level)
Deterministic content (seeded), 200-1500 bytes of pseudo-text per file."""
import os, sys, random
dest, layout, n = sys.argv[1], sys.argv[2], int(sys.argv[3])
rnd = random.Random(42)
words = [''.join(rnd.choice('abcdefghijklmnopqrstuvwxyz') for _ in range(rnd.randint(2, 9))) for _ in range(2000)]
def body(i):
    r = random.Random(i)
    return (' '.join(r.choice(words) for _ in range(r.randint(30, 200))) + '\n').encode()
def w(p, i):
    with open(p, 'wb') as f: f.write(body(i))
os.makedirs(dest, exist_ok=True)
if layout == 'flat1k':
    for i in range(n):
        d = f'{dest}/d{i//1000:04d}'
        if i % 1000 == 0: os.makedirs(d)
        w(f'{d}/f{i%1000:04d}.txt', i)
elif layout == 'nested':
    for i in range(n):
        d = f'{dest}/a{i//10000:03d}/b{(i//100)%100:03d}'
        if i % 100 == 0: os.makedirs(d, exist_ok=True)
        w(f'{d}/f{i%100:03d}.txt', i)
elif layout == 'wide':
    for i in range(n): w(f'{dest}/f{i:07d}.txt', i)
elif layout == 'deep':
    d = dest
    for lvl in range(n):
        d = f'{d}/l{lvl:03d}'; os.makedirs(d)
        for k in range(5): w(f'{d}/f{k}.txt', lvl * 5 + k)
