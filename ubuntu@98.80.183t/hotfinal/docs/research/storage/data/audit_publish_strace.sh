#!/bin/sh
# Syscalls of the first `vcs publish` to a filesystem remote: reads per local object, fsyncs at the remote.
# usage: audit_publish_strace.sh <vcs> <datagen> <workdir> <files>
set -e
VCS=$1; DG=$2; W=$3; N=$4; rm -rf "$W"; mkdir -p "$W/cfg"
export VCS_AUTHOR_NAME=B VCS_AUTHOR_EMAIL=b@example.invalid VCS_CONFIG_HOME="$W/cfg"
$DG flat "$N" "$W/r" >/dev/null; cd "$W/r"
$VCS init >/dev/null; $VCS add . >/dev/null; $VCS commit -m c >/dev/null
$VCS remote set "$W/remote" >/dev/null
strace -f -e trace=openat,fsync,rename,link -o "$W/publish.trace" $VCS publish >/dev/null
echo "remote objects: $(find "$W/remote/objects" -type f | wc -l)"
echo "fsync calls:    $(grep -c 'fsync(' "$W/publish.trace")"
echo "link() calls:   $(grep -c ' link(' "$W/publish.trace")"
echo "opens of local object files (read):  $(grep 'openat' "$W/publish.trace" | grep '/r/.vcs/objects/' | grep -vc 'O_WRONLY')"
echo "distinct local object files opened:  $(grep 'openat' "$W/publish.trace" | grep '/r/.vcs/objects/' | grep -v 'O_WRONLY' | sed 's/.*openat([^"]*"\([^"]*\)".*/\1/' | sort -u | wc -l)"
