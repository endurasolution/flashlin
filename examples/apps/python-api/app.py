"""Minimal Python app for FlashLin's [[apps]] manager — standard library only.

FlashLin passes PORT and HOST; it proxies /py/* here (see examples/apps.toml).
"""
import json
import os
import signal
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/health":
            body = b"ok"
        else:
            body = json.dumps(
                {
                    "app": os.environ["FLASHLIN_APP"],
                    "path": self.path,
                    "client": self.headers.get("x-forwarded-for"),
                }
            ).encode()
        self.send_response(200)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        print(fmt % args)


server = ThreadingHTTPServer((os.environ["HOST"], int(os.environ["PORT"])), Handler)
# FlashLin sends SIGTERM on stop/restart.
signal.signal(signal.SIGTERM, lambda *_: sys.exit(0))
print(f"listening on {os.environ['HOST']}:{os.environ['PORT']}")
server.serve_forever()
