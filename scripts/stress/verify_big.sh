#!/usr/bin/env bash
# The prebuilt big files after a run: clone zone `big` straight from the server's store directory and compare the
# sha256 and size of every file with the manifest build_base.sh recorded for the last version.
#   verify_big.sh STOREDIR MANIFEST.json WORKDIR      env: VCS_BIN
set -uo pipefail
S=${1:?store}; M=${2:?manifest}; W=${3:?workdir}; HERE=$(cd "$(dirname "$0")" && pwd); export PATH=${VCS_BIN:?VCS_BIN}:$PATH
export VCS_AUTHOR_NAME=v VCS_AUTHOR_EMAIL=v@x XDG_CACHE_HOME=$W/cache
rm -rf "$W"; mkdir -p "$W"
/usr/bin/time -f "clone zone big: %e s wall, %M KB" vcs clone "$S" "$W/c" --zone big 2>&1 | tail -1
/usr/bin/time -f "hash: %e s wall" python3 "$HERE/bigfiles.py" hash "$W/c/big" "$W/got.json" 2>&1 | tail -2
python3 - "$M" "$W/got.json" <<'PY'
import json,sys
want,got=json.load(open(sys.argv[1])),json.load(open(sys.argv[2]))
eq=sum(1 for k,v in want.items() if got.get(k)==v)
print(f"big files: {eq}/{len(want)} equal in sha256 and size; extra files: {sorted(set(got)-set(want))}")
sys.exit(0 if eq==len(want) else 1)
PY
rm -rf "$W/c"
