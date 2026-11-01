#!/usr/bin/env python3
"""Counting TCP forwarder (research benchmark helper): usage ci_tcpcount.py LISTEN_PORT UPSTREAM_HOST UPSTREAM_PORT
Counts connections and bytes each way. Control: connect and send "STATS\\n" / "RESET\\n" on CTRL_PORT = LISTEN_PORT+1."""
import asyncio, json, sys

LP, UH, UP = int(sys.argv[1]), sys.argv[2], int(sys.argv[3])
C = {"connections": 0, "bytes_up": 0, "bytes_down": 0}


async def pipe(r, w, key):
    try:
        while True:
            d = await r.read(65536)
            if not d:
                break
            C[key] += len(d)
            w.write(d)
            await w.drain()
    except Exception:
        pass
    finally:
        try:
            w.close()
        except Exception:
            pass


async def conn(r, w):
    C["connections"] += 1
    try:
        ur, uw = await asyncio.open_connection(UH, UP)
    except Exception:
        w.close()
        return
    await asyncio.gather(pipe(r, uw, "bytes_up"), pipe(ur, w, "bytes_down"))


async def ctrl(r, w):
    line = (await r.readline()).decode().strip()
    if line == "RESET":
        C.update(connections=0, bytes_up=0, bytes_down=0)
    w.write((json.dumps(C) + "\n").encode())
    await w.drain()
    w.close()


async def main():
    s1 = await asyncio.start_server(conn, "127.0.0.1", LP, backlog=512)
    s2 = await asyncio.start_server(ctrl, "127.0.0.1", LP + 1)
    async with s1, s2:
        await asyncio.gather(s1.serve_forever(), s2.serve_forever())

asyncio.run(main())
