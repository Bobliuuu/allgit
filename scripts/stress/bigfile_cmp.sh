#!/usr/bin/env bash
# One big binary (a copy of a file of the stress base, 1-5 GiB), git against vcs (through vcs-server) on the same bytes:
# add, commit, push of v1; then 3 MiB overwritten and 1 MiB inserted -> v2: add, commit, push; clone of v2; store sizes.
#   bigfile_cmp.sh BASEDIR OUTDIR FILE     (FILE e.g. $BASE/seed/big/big03.bin)    env: VCS_BIN, ONLY_VCS=1 (append the vcs rows to an existing OUTDIR)
set -uo pipefail
B=${1:?base}; O=${2:?out}; F=${3:?file}; export PATH=${VCS_BIN:?VCS_BIN}:$PATH
export VCS_AUTHOR_NAME=b VCS_AUTHOR_EMAIL=b@x VCS_TOKEN=tok-admin VCS_PRINCIPAL=admin GIT_AUTHOR_NAME=b GIT_AUTHOR_EMAIL=b@x GIT_COMMITTER_NAME=b GIT_COMMITTER_EMAIL=b@x
export XDG_CACHE_HOME=$O/cache; [ -n "${ONLY_VCS:-}" ] || rm -rf "$O"; mkdir -p "$O"; R=$O/results.tsv; [ -n "${ONLY_VCS:-}" ] || : > "$R"
m() { local tool=$1 op=$2; shift 2; /usr/bin/time -f "$tool\t$op\t%e\t%U\t%S\t%M\t%x" -a -o "$R" "$@" > "$O/$tool-$op.out" 2> "$O/$tool-$op.err" || true; }
mutate() { python3 - "$1" <<'PY'
import os,sys,random
p=sys.argv[1]; r=random.Random(5); sz=os.path.getsize(p)
with open(p,"r+b") as f:
    for _ in range(3): f.seek(r.randrange(sz-(1<<20))); f.write(os.urandom(1<<20))
tmp=p+".tmp"; off=r.randrange(sz-(2<<20))
with open(p,"rb") as a, open(tmp,"wb") as b:
    left=off
    while left: x=a.read(min(left,64<<20)); b.write(x); left-=len(x)
    b.write(os.urandom(1<<20))
    while True:
        x=a.read(64<<20)
        if not x: break
        b.write(x)
os.replace(tmp,p)
PY
}
sha() { python3 - "$1" <<'PY'
import hashlib,sys
x=hashlib.sha256()
with open(sys.argv[1],'rb') as f:
    for b in iter(lambda:f.read(16<<20),b''): x.update(b)
print(x.hexdigest())
PY
}
# ---- git
if [ -z "${ONLY_VCS:-}" ]; then
git init -q --bare "$O/git-remote.git"; mkdir "$O/g"; cd "$O/g"; git init -q -b main; git remote add origin "$O/git-remote.git"
cp "$F" big.bin
m git add_v1 git add big.bin
m git commit_v1 git commit -qm v1
m git push_v1 git push -q origin main
mutate big.bin
m git add_v2 git add big.bin
m git commit_v2 git commit -qm v2
m git push_v2 git push -q origin main
m git clone_v2 git clone -q "file://$O/git-remote.git" "$O/g-clone"
du -sk "$O/git-remote.git" | awk '{print "git\tremote_kb\t" $1}' >> "$R"
[ "$(sha "$O/g/big.bin")" = "$(sha "$O/g-clone/big.bin")" ] && echo -e "git\tclone_hash_equal\tTrue" >> "$R" || echo -e "git\tclone_hash_equal\tFalse" >> "$R"
rm -rf "$O/g-clone" "$O/g" "$O/git-remote.git"
fi
# ---- vcs through a server
cd "$O"; vcs-server --root "$O/vcs-store" --tokens "$B/tokens" --listen 127.0.0.1:8494 >"$O/server.log" 2>&1 & SP=$!
until curl -sf http://127.0.0.1:8494/healthz >/dev/null; do sleep 0.2; done
vcs init v >/dev/null; cd v; vcs remote set http://127.0.0.1:8494 >/dev/null
cp "$F" big.bin
m vcs add_v1 vcs add big.bin
m vcs commit_v1 vcs commit -m v1
m vcs push_v1 vcs publish
mutate big.bin
m vcs add_v2 vcs add big.bin
m vcs commit_v2 vcs commit -m v2
m vcs push_v2 vcs publish
m vcs clone_v2 vcs clone http://127.0.0.1:8494 "$O/v-clone"
du -sk "$O/vcs-store" | awk '{print "vcs\tremote_kb\t" $1}' >> "$R"
[ "$(sha "$O/v/big.bin")" = "$(sha "$O/v-clone/big.bin")" ] && echo -e "vcs\tclone_hash_equal\tTrue" >> "$R" || echo -e "vcs\tclone_hash_equal\tFalse" >> "$R"
kill $SP; wait $SP 2>/dev/null; rm -rf "$O/v-clone" "$O/v" "$O/vcs-store"
cat "$R"
