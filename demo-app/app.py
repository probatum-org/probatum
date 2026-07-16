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

# Env switches let this one mock play several failure stories:
#   WAL_DIR=/nonexistent  -> crash at boot (missing segment)
#   DEGRADE=1             -> become ready, then log an ERROR (degrades after readiness)
#   LOG_FILE=path         -> also append log lines to an external file (log: checks)
#   HANG=1                -> boot fine but never open the port (readiness timeout)
WAL_DIR = os.environ.get("WAL_DIR") or os.path.join(os.path.dirname(__file__), "data", "wal")
SEGMENTS = ["0001", "0002", "0003", "0004"]


def log(level, msg):
    line = f"{level} {msg}"
    print(line, flush=True)
    lf = os.environ.get("LOG_FILE")
    if lf:
        with open(lf, "a") as f:
            f.write(line + "\n")


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
    if os.environ.get("HANG"):
        log("INFO", "HANG=1: boot done but never opening the port")
        time.sleep(3600)
    log("INFO", "listening on 127.0.0.1:8087")
    if os.environ.get("DEGRADE"):
        import threading
        threading.Timer(
            0.3, lambda: log("ERROR", "cache backend unreachable, serving stale data")
        ).start()
    HTTPServer(("127.0.0.1", 8087), Handler).serve_forever()
