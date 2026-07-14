#!/usr/bin/env python3
"""Demo app: an event-sourced-ish store that replays its WAL at boot.

The bug (on purpose): unit tests mock the store, so they all pass —
but the real boot replays the WAL from disk, and segment 0004 is missing.
Exactly the class of failure Probatum exists to surface.
"""
import json
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, HTTPServer

WAL_DIR = os.path.join(os.path.dirname(__file__), "data", "wal")
SEGMENTS = ["0001", "0002", "0003", "0004"]


def log(level, msg):
    print(f"{level} {msg}", flush=True)


def replay_wal():
    log("INFO", "store::replay starting WAL replay")
    state = {}
    for seg in SEGMENTS:
        path = os.path.join(WAL_DIR, f"segment-{seg}.json")
        if not os.path.exists(path):
            log("ERROR", f"store::replay segment {seg} not found in data/wal")
            log("FATAL", f"boot aborted: cannot rebuild state without segment {seg}")
            sys.exit(1)
        with open(path) as f:
            for event in json.load(f):
                state[event["key"]] = event["value"]
        log("INFO", f"store::replay segment {seg} applied")
    log("INFO", f"store::replay done ({len(state)} keys)")
    return state


class Handler(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/healthz":
            self._reply(200, {"status": "ok"})
        elif self.path == "/api/version":
            self._reply(200, {"version": "1.3.0", "keys": len(STATE)})
        else:
            self._reply(404, {"error": "not found"})

    def _reply(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt, *args):
        log("INFO", f"http {fmt % args}")


if __name__ == "__main__":
    STATE = replay_wal()
    time.sleep(0.2)
    log("INFO", "listening on 127.0.0.1:8087")
    HTTPServer(("127.0.0.1", 8087), Handler).serve_forever()
