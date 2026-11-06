#!/usr/bin/env python3
"""Minimal inotify-backed fsmonitor for git (hook protocol v2), because git's builtin `fsmonitor--daemon` exists only on macOS and
Windows (verified on git 2.43 Linux: "not supported on this platform") and watchman is not installed on the bench box.
  serve <root> <sock>            daemon: watches every directory under root (not .git/.vcs), answers queries on a unix socket
  hook  <sock> <version> <token> hook for core.fsmonitor: prints "<token>\\0<changed path>\\0..." (or "/" = everything on unknown token)
Token = "<epoch>:<seq>". A daemon restart or an inotify queue overflow changes the epoch, which makes git rescan once.
This stands in for watchman in the "git fsmonitor + untrackedCache" arm; its CPU is counted by bench_status_fair.py."""
import ctypes, os, selectors, socket, struct, sys, time

IN = dict(MODIFY=0x2, ATTRIB=0x4, CLOSE_WRITE=0x8, MOVED_FROM=0x40, MOVED_TO=0x80, CREATE=0x100, DELETE=0x200, DELETE_SELF=0x400,
          Q_OVERFLOW=0x4000, ISDIR=0x40000000)
MASK = IN["MODIFY"] | IN["ATTRIB"] | IN["CLOSE_WRITE"] | IN["MOVED_FROM"] | IN["MOVED_TO"] | IN["CREATE"] | IN["DELETE"] | IN["DELETE_SELF"]
SKIP = {".git", ".vcs"}


def serve(root, sock_path):
    libc = ctypes.CDLL("libc.so.6", use_errno=True)
    fd = libc.inotify_init1(os.O_NONBLOCK)
    wd_path = {}
    epoch = str(int(time.time() * 1000))
    seq = 0
    events = []  # (seq, relpath)

    def watch(d):
        wd = libc.inotify_add_watch(fd, d.encode(), MASK)
        if wd >= 0:
            wd_path[wd] = d
        for e in os.scandir(d):
            if e.is_dir(follow_symlinks=False) and e.name not in SKIP:
                watch(e.path)

    watch(root)
    srv = socket.socket(socket.AF_UNIX)
    if os.path.exists(sock_path):
        os.unlink(sock_path)
    srv.bind(sock_path)
    srv.listen(16)
    srv.setblocking(False)
    sel = selectors.DefaultSelector()
    sel.register(fd, selectors.EVENT_READ, "ino")
    sel.register(srv, selectors.EVENT_READ, "srv")
    while True:
        for key, _ in sel.select():
            if key.data == "ino":
                try:
                    buf = os.read(fd, 1 << 20)
                except BlockingIOError:
                    continue
                i = 0
                while i < len(buf):
                    wd, mask, _c, ln = struct.unpack_from("iIII", buf, i)
                    name = buf[i + 16:i + 16 + ln].rstrip(b"\0").decode(errors="replace")
                    i += 16 + ln
                    if mask & IN["Q_OVERFLOW"]:
                        epoch = str(int(time.time() * 1000))
                        events.clear()
                        continue
                    d = wd_path.get(wd)
                    if d is None or name in SKIP:
                        continue
                    seq += 1
                    events.append((seq, os.path.relpath(os.path.join(d, name), root)))
                    if mask & IN["ISDIR"] and mask & (IN["CREATE"] | IN["MOVED_TO"]):
                        try:
                            watch(os.path.join(d, name))
                        except OSError:
                            pass
                if len(events) > 2_000_000:
                    epoch = str(int(time.time() * 1000))
                    events.clear()
            else:
                c, _ = srv.accept()
                c.settimeout(2)
                tok = c.recv(256).decode()
                ep, _, s = tok.partition(":")
                new = f"{epoch}:{seq}"
                if ep != epoch or not s.isdigit():
                    out = new.encode() + b"\0/\0"
                else:
                    since = int(s)
                    out = new.encode() + b"\0" + b"".join(p.encode() + b"\0" for q, p in events if q > since)
                c.sendall(out)
                c.close()


def hook(sock_path, version, token):
    c = socket.socket(socket.AF_UNIX)
    c.connect(sock_path)
    c.sendall(token.encode())
    c.shutdown(socket.SHUT_WR)
    out = b""
    while True:
        b = c.recv(1 << 20)
        if not b:
            break
        out += b
    sys.stdout.buffer.write(out)


if __name__ == "__main__":
    if sys.argv[1] == "serve":
        serve(os.path.abspath(sys.argv[2]), sys.argv[3])
    else:
        hook(sys.argv[2], sys.argv[3], sys.argv[4])
