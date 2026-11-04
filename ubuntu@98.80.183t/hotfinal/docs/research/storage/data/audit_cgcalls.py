#!/usr/bin/env python3
"""Sum call counts of functions matching a regex in callgrind out files. usage: audit_cgcalls.py <regex> <files...>"""
import re, sys
rx = re.compile(sys.argv[1])
for f in sys.argv[2:]:
    names, tot, cur_calls, target = {}, {}, None, None
    for l in open(f, errors="replace"):
        m = re.match(r"c?fn=\((\d+)\)(?: (.*))?", l)
        if m:
            if m.group(2): names[m.group(1)] = m.group(2)
            if l.startswith("cfn="): target = m.group(1)
            continue
        m = re.match(r"calls=(\d+)", l)
        if m and target is not None:
            n = names.get(target, target)
            if rx.search(n): tot[n] = tot.get(n, 0) + int(m.group(1))
    print(f, tot)
