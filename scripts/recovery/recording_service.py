#!/usr/bin/env python3
"""BN-022 final-candidate recovery harness: loopback recording service.

Disposable-fixture HTTP service used ONLY by the WP2.8/final-candidate
operational recovery harness. Binds 127.0.0.1, records every request
(method, path, selected headers including x-bonsai-routing-token) as
JSON lines to the file given by RECORDER_LOG, and responds according to
RECORDER_MODE:

  ok         -> 200 {"ok": true, ...}  (default)
  redirect   -> 3xx with Location: $RECORDER_LOCATION

Never logs bodies. Synthetic loopback only.
"""
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

LOG = os.environ["RECORDER_LOG"]
MODE = os.environ.get("RECORDER_MODE", "ok")
LOCATION = os.environ.get("RECORDER_LOCATION", "")
STATUS = int(os.environ.get("RECORDER_STATUS", "302"))


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _handle(self):
        length = int(self.headers.get("Content-Length") or 0)
        if length:
            self.rfile.read(length)
        rec = {
            "method": self.command,
            "path": self.path,
            "token": self.headers.get("x-bonsai-routing-token"),
            "authorization": "present" if self.headers.get("authorization") else None,
        }
        with open(LOG, "a") as f:
            f.write(json.dumps(rec) + "\n")
        if MODE == "redirect":
            self.send_response(STATUS)
            self.send_header("Location", LOCATION)
            self.send_header("Content-Length", "0")
            self.end_headers()
        else:
            body = json.dumps({"ok": True, "projection_semantics_version": 1,
                               "durable_sequence": 0, "events": []}).encode()
            self.send_response(200)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

    do_GET = do_POST = do_PUT = do_DELETE = do_PATCH = _handle

    def log_message(self, *a):
        pass


if __name__ == "__main__":
    port = int(sys.argv[1])
    ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
