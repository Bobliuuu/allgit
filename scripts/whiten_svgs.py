#!/usr/bin/env python3
"""Force light/white rendering: strip prefers-color-scheme:dark blocks, make backgrounds white."""
import re, sys, pathlib

def strip_media(s):
    out, i = [], 0
    pat = re.compile(r"@media\s*\(\s*prefers-color-scheme\s*:\s*dark\s*\)\s*\{")
    while True:
        m = pat.search(s, i)
        if not m:
            out.append(s[i:]); break
        out.append(s[i:m.start()])
        depth, j = 1, m.end()
        while depth and j < len(s):
            depth += (s[j] == "{") - (s[j] == "}")
            j += 1
        i = j
    return "".join(out)

changed = 0
for p in pathlib.Path(sys.argv[1]).rglob("*.svg"):
    s = p.read_text()
    n = strip_media(s)
    n = re.sub(r"(\.bg\s*\{\s*fill:)#[0-9a-fA-F]{3,6}", r"\1#ffffff", n)
    if n != s:
        p.write_text(n); changed += 1
print("changed", changed)
