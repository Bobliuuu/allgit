#!/usr/bin/env python3
"""Counting HTTP/1.1 forward proxy for an S3-compatible server (research benchmark helper).

usage: s3count_proxy.py LISTEN_PORT UPSTREAM_HOST UPSTREAM_PORT

Counts requests by method and payload bytes (request body up, response body down).
Control endpoint (answered by the proxy, not forwarded):
  GET  /__proxy/stats   -> JSON totals
  POST /__proxy/reset   -> zero the counters
Request and response bodies are streamed verbatim; Content-Length and chunked bodies are both handled.
Used only for the counting pass of the benchmark: timed runs go direct to the object store.
"""
import http.client
import json
import socketserver
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LISTEN = int(sys.argv[1])
UP_HOST, UP_PORT = sys.argv[2], int(sys.argv[3])
LOCK = threading.Lock()
C = {"requests": 0, "by_method": {}, "bytes_up": 0, "bytes_down": 0, "status": {}}


def reset():
    with LOCK:
        C.update(requests=0, by_method={}, bytes_up=0, bytes_down=0, status={})


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def _ctl(self):
        if self.path == "/__proxy/stats":
            with LOCK:
                b = json.dumps(C).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(b)))
            self.end_headers()
            self.wfile.write(b)
            return True
        if self.path == "/__proxy/reset":
            reset()
            self.send_response(200)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return True
        return False

    def _body(self):
        te = self.headers.get("Transfer-Encoding", "")
        if "chunked" in te.lower():
            out = bytearray()
            while True:
                size = int(self.rfile.readline().split(b";")[0].strip() or b"0", 16)
                if size == 0:
                    while self.rfile.readline() not in (b"\r\n", b"\n", b""):
                        pass
                    break
                out += self.rfile.read(size)
                self.rfile.readline()
            return bytes(out)
        n = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(n) if n else b""

    def _do(self):
        if self._ctl():
            return
        if self.headers.get("Expect", "").lower() == "100-continue":
            self.send_response_only(100)
            self.end_headers()
        body = self._body()
        hdrs = {k: v for k, v in self.headers.items()
                if k.lower() not in ("transfer-encoding", "expect", "connection", "proxy-connection")}
        hdrs["Content-Length"] = str(len(body))
        conn = http.client.HTTPConnection(UP_HOST, UP_PORT, timeout=600)
        try:
            conn.request(self.command, self.path, body=body if (body or self.command in ("PUT", "POST")) else None,
                         headers=hdrs)
            r = conn.getresponse()
            data = r.read() if self.command != "HEAD" else b""
            with LOCK:
                C["requests"] += 1
                C["by_method"][self.command] = C["by_method"].get(self.command, 0) + 1
                C["bytes_up"] += len(body)
                C["bytes_down"] += len(data)
                C["status"][str(r.status)] = C["status"].get(str(r.status), 0) + 1
            self.send_response_only(r.status, r.reason)
            for k, v in r.getheaders():
                if k.lower() in ("transfer-encoding", "connection", "content-length"):
                    continue
                self.send_header(k, v)
            clen = r.getheader("Content-Length")
            self.send_header("Content-Length", clen if (self.command == "HEAD" and clen) else str(len(data)))
            self.end_headers()
            if data:
                self.wfile.write(data)
        finally:
            conn.close()

    do_GET = do_PUT = do_POST = do_HEAD = do_DELETE = _do


class S(ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 512


S(("127.0.0.1", LISTEN), H).serve_forever()
