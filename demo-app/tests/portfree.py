#!/usr/bin/env python3
"""Exit 0 if nothing listens on 127.0.0.1:8087, exit 1 otherwise.
Used by the dogfooding suite to assert probatum freed the port."""
import socket
import sys

sys.exit(1 if socket.socket().connect_ex(("127.0.0.1", 8087)) == 0 else 0)
