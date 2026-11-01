#!/usr/bin/env bash
# Build a small history in a REAL local p4d and record its output as an import-p4 fixture directory,
# plus ground truth (`p4 sync @N` manifests) to compare the imported trees against.
#
# usage: scripts/p4_capture.sh <out-dir>      env: P4 (p4 client), P4D (server), PORT (default 11667)
# Output layout (extends docs/import-p4.md):
#   <out>/changes.txt                 p4 changes -l            (text; used by the importer; includes the pending change)
#   <out>/changes.marshal             p4 -G changes -l         (reference only)
#   <out>/describe/N.marshal          p4 -G describe -s N      (used by the importer)
#   <out>/describe-text/N.txt         p4 describe -s N         (text form; the test copies it to describe/ to test that parser)
#   <out>/print/N/<depot path>        p4 print -q //depot/path#rev for every add/edit/branch/integrate/move-add
#   <out>/expected/N.manifest         ground truth: `p4 sync //depot/...@N` into an empty client, one line per entry:
#                                     "<mode> <path> <sha256> <size>" (mode 644/755) or "L <path> <target>"
# The server is stopped and its root deleted at the end. Needs python3 and a p4d/p4 pair (see docs/research/perforce-benchmark.md).
set -euo pipefail
OUT="$(cd "$(dirname "$1")" && pwd)/$(basename "$1")"
P4="${P4:-$HOME/p4/bin/p4}"
P4D="${P4D:-$HOME/p4/bin/p4d}"
PORT="${PORT:-11667}"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/p4cap.XXXXXX")"
export P4PORT="127.0.0.1:$PORT" P4CONFIG= P4ENVIRO=/dev/null P4TICKETS="$WORK/tickets"
mkdir -p "$WORK/srv" "$WORK/alice" "$WORK/bob"
cleanup() { P4USER=alice "$P4" admin stop >/dev/null 2>&1 || true; sleep 1; rm -rf "$WORK"; }
trap cleanup EXIT

( cd "$WORK/srv" && exec "$P4D" -r "$WORK/srv" -J off -L /dev/null -p "$P4PORT" >/dev/null 2>&1 ) &
for _ in $(seq 1 100); do "$P4" -u alice info >/dev/null 2>&1 && break; sleep 0.1; done
# p4d 2026.1 starts with no access until the first user (the superuser) has a password; create the others as super
printf 'Capturepass1!\nCapturepass1!\n' | P4USER=alice "$P4" passwd >/dev/null
echo 'Capturepass1!' | P4USER=alice "$P4" login >/dev/null
P4USER=alice "$P4" user -o bob | P4USER=alice "$P4" user -f -i >/dev/null
printf 'Capturepass1!\nCapturepass1!\n' | P4USER=alice "$P4" passwd bob >/dev/null
# an initial password set by the super-user is expired: bob changes it (login with the old one first)
echo 'Capturepass1!' | P4USER=bob "$P4" login >/dev/null 2>&1 || true
printf 'Capturepass1!\nCapturepass2!\nCapturepass2!\n' | P4USER=bob "$P4" passwd >/dev/null
for u in alice bob; do
  pw='Capturepass1!'; [ $u = bob ] && pw='Capturepass2!'
  echo "$pw" | P4USER=$u "$P4" login >/dev/null
  P4USER=$u P4CLIENT=${u}_ws "$P4" client -o | sed "s|^Root:.*|Root:\t$WORK/$u|" | P4USER=$u P4CLIENT=${u}_ws "$P4" client -i >/dev/null
done
A() { ( cd "$WORK/alice" && P4USER=alice P4CLIENT=alice_ws "$P4" "$@" ); }
B() { ( cd "$WORK/bob" && P4USER=bob P4CLIENT=bob_ws "$P4" "$@" ); }
# submit with a description (given as a quoted string)
A_submit() { A submit -d "$1" >/dev/null; }
B_submit() { B submit -d "$1" >/dev/null; }

cd "$WORK/alice"
mkdir -p proj/src proj/bin proj/data
printf 'Project readme\nsecond line\n' > proj/README.txt
printf '#include <stdio.h>\nint main(void){return 0;}\n' > proj/src/main.c
printf '#!/bin/sh\necho run\n' > proj/bin/run.sh
head -c 3000 /dev/urandom > proj/data/blob.bin
ln -s README.txt proj/link
: > proj/empty.txt
printf 'with space\n' > "proj/sp ace.txt"
A add -t text+x proj/bin/run.sh >/dev/null
A add -t binary proj/data/blob.bin >/dev/null
A add -t symlink proj/link >/dev/null
A add proj/README.txt proj/src/main.c proj/empty.txt "proj/sp ace.txt" >/dev/null
A_submit "Initial import of proj
with a second description line"

B sync >/dev/null 2>&1
B edit proj/src/main.c >/dev/null
printf '#include <stdio.h>\nint main(void){puts("hi");return 0;}\n' > "$WORK/bob/proj/src/main.c"
mkdir -p "$WORK/bob/proj/src"
printf 'int util(void){return 1;}\n' > "$WORK/bob/proj/src/util.c"
B add proj/src/util.c >/dev/null
B_submit "Greet and add util"

A sync >/dev/null 2>&1
A edit proj/src/util.c >/dev/null
A move proj/src/util.c proj/src/lib/util.c >/dev/null
printf 'int util(void){return 2;}\n' > proj/src/lib/util.c
A_submit "Move util into lib"

B sync >/dev/null 2>&1
B integ //depot/proj/... //depot/rel/... >/dev/null
B_submit "Branch proj to rel"

A sync >/dev/null 2>&1
A edit proj/src/main.c >/dev/null
printf '#include <stdio.h>\nint main(void){puts("hello");return 0;}\n' > proj/src/main.c
A_submit "Hello in proj"
A integ //depot/proj/... //depot/rel/... >/dev/null
A resolve -at >/dev/null 2>&1 || true
A_submit "Integrate proj into rel"

A sync >/dev/null 2>&1
A delete proj/link >/dev/null
A edit proj/data/blob.bin >/dev/null
head -c 2000 /dev/urandom > proj/data/blob.bin
A edit -t text proj/bin/run.sh >/dev/null
A_submit "Drop link, new blob, run.sh loses +x"

B sync >/dev/null 2>&1
B edit -t text+x proj/README.txt >/dev/null
printf 'Project readme\nnow executable\n' > "$WORK/bob/proj/README.txt"
printf 'a\r\nb' > "$WORK/bob/proj/crlf.txt"
head -c 500 /dev/urandom > "$WORK/bob/proj/bin/tool.bin"
B add -t text proj/crlf.txt >/dev/null
B add -t binary+x proj/bin/tool.bin >/dev/null
B_submit "Executable readme, crlf file, binary tool"

A sync >/dev/null 2>&1
A delete proj/src/lib/util.c >/dev/null
printf 'now a plain file\n' > proj/link
A add -t text proj/link >/dev/null
A_submit "Remove util; re-add link as a regular file"

cd "$WORK/alice"
A sync >/dev/null 2>&1
printf 'plain\n' > 'proj/we@ird#na%me*.txt'
printf 'caf\xc3\xa9\n' > "proj/caf$(printf '\xc3\xa9').txt"
printf 'Id: $Id$\nRev: $Revision$\n' > proj/kw.txt
printf '\xff\xfeh\x00i\x00\n\x00' > proj/u16.txt
A add -f -t text 'proj/we@ird#na%me*.txt' >/dev/null
A add -f -t text "proj/caf$(printf '\xc3\xa9').txt" >/dev/null
A add -t text+k proj/kw.txt >/dev/null
A add -t utf16 proj/u16.txt >/dev/null
A_submit "Odd names, keywords, utf16"

# a pending change (must be ignored by the importer)
A edit proj/README.txt >/dev/null
printf 'pending edit\n' > proj/README.txt
A change -o | sed 's/<enter description here>/pending work/' | A change -i >/dev/null

# ---- record ----
rm -rf "$OUT"; mkdir -p "$OUT/describe" "$OUT/describe-text" "$OUT/expected"
A changes -l > "$OUT/changes.txt"
A -G changes -l > "$OUT/changes.marshal"
mapfile -t CHS < <(A changes -s submitted | awk '{print $2}' | sort -n)
for n in "${CHS[@]}"; do
  A -G describe -s "$n" > "$OUT/describe/$n.marshal"
  A describe -s "$n" > "$OUT/describe-text/$n.txt"
  # list "depotFile rev action" from the real marshalled output and print each content-bearing revision
  python3 -I - "$OUT/describe/$n.marshal" > "$WORK/files.$n" <<'E'
import marshal, sys
with open(sys.argv[1], "rb") as f:
    d = marshal.load(f)
u = lambda b: b.decode("utf-8", "surrogateescape") if isinstance(b, bytes) else b
d = {u(k): u(v) for k, v in d.items()}
sys.stdout.reconfigure(encoding="utf-8", errors="surrogateescape")
i = 0
while f"depotFile{i}" in d:
    print(d[f"depotFile{i}"], d[f"rev{i}"], d[f"action{i}"], sep="\t")
    i += 1
E
  while IFS=$'\t' read -r path rev action; do
    case "$action" in
      add|edit|branch|integrate|import|move/add)
        dest="$OUT/print/$n/${path#//}"; mkdir -p "$(dirname "$dest")"
        A print -q "$path#$rev" > "$dest" ;;
    esac
  done < "$WORK/files.$n"
  # ground truth: fresh client, sync @n
  v="v$n"; mkdir -p "$WORK/$v"
  P4USER=alice P4CLIENT=$v "$P4" client -o | sed "s|^Root:.*|Root:\t$WORK/$v|" | P4USER=alice P4CLIENT=$v "$P4" client -i >/dev/null
  P4USER=alice P4CLIENT=$v "$P4" -q sync "//depot/...@$n" >/dev/null
  python3 -I - "$WORK/$v" > "$OUT/expected/$n.manifest" <<'E'
import hashlib, os, sys
root = sys.argv[1]
out = []
for d, ds, fs in os.walk(root):
    for name in ds + fs:
        p = os.path.join(d, name)
        rel = os.path.relpath(p, root)
        if os.path.islink(p):
            out.append(f"L {rel} {os.readlink(p)}")
        elif os.path.isfile(p):
            b = open(p, "rb").read()
            m = "755" if os.stat(p).st_mode & 0o100 else "644"
            out.append(f"{m} {rel} {hashlib.sha256(b).hexdigest()} {len(b)}")
for l in sorted(out, key=lambda s: s.split(" ")[1]):
    print(l)
E
done
echo "captured ${#CHS[@]} submitted changes into $OUT"
