#!/bin/sh
# usage: build_doc.sh head.md tail.md out.md  (results = every data/walgit-bench/res-*.json, 10k then 100k)
D=docs/research/data/walgit-bench
{ cat "$1"; echo "## Results (clean pass, gc.auto=0)"; for f in $D/res-*-10000.json $D/res-*-100000.json; do [ -f "$f" ] && python3 scripts/walgit_bench/report.py "$f"; done; cat "$2"; } > "$3"
