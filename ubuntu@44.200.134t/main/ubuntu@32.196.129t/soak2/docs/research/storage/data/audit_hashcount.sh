#!/bin/sh
# Count BLAKE3 finalizations (= completed hashes) and bytes per CLI operation with callgrind.
# usage: audit_hashcount.sh <vcs> <workdir>   (valgrind required; small trees only, callgrind is ~50x slower)
set -e
VCS=$1; W=$2; rm -rf "$W"; mkdir -p "$W/cfg" "$W/r"; cd "$W/r"
export VCS_AUTHOR_NAME=B VCS_AUTHOR_EMAIL=b@example.invalid VCS_CONFIG_HOME="$W/cfg"
$VCS init >/dev/null
mkdir -p d1 d2
for i in 1 2 3 4 5 6 7 8 9 10; do head -c 600 /dev/urandom > d1/f$i; head -c 600 /dev/urandom > d2/g$i; done
head -c 2097152 /dev/urandom > big.bin
sleep 2.5
cg() { name=$1; shift; valgrind --tool=callgrind --callgrind-out-file="$W/cg.$name" $VCS "$@" >/dev/null 2>&1; }
cg add_all add .
cg commit commit -m first
cg add_again add .
cg status status
echo edit > d1/f3
cg add_one add d1/f3
cg commit_one commit -m second
