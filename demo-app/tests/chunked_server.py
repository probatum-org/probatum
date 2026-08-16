#!/usr/bin/env python3
"""Answers with Transfer-Encoding: chunked, splitting the body mid-token.

Node, Go and nginx proxies all answer chunked when the length isn't known up
front. Undecoded, the chunk sizes land inside the body and a `contains` rule
misses a string that is really there — a false failure. This fixture makes
that case reproducible.
"""
import socket
import sys

PORT = int(sys.argv[1]) if len(sys.argv) > 1 else 8094
HEAD, TAIL = '{"ver', 'sion": "1.3.0"}'  # "version" straddles the boundary

srv = socket.socket()
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", PORT))
srv.listen(8)
print(f"chunked fixture listening on 127.0.0.1:{PORT}", flush=True)

while True:
    conn, _ = srv.accept()
    # Read the whole request head before answering: closing with unread bytes
    # in the socket makes the kernel send RST, and a client that has not read
    # the answer yet sees "connection reset" — a flake seen on a slow CI runner.
    req = b""
    while b"\r\n\r\n" not in req:
        chunk = conn.recv(4096)
        if not chunk:
            break
        req += chunk
    conn.sendall(
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n"
        b"Transfer-Encoding: chunked\r\n\r\n"
        + f"{len(HEAD):x}\r\n{HEAD}\r\n{len(TAIL):x}\r\n{TAIL}\r\n0\r\n\r\n".encode()
    )
    conn.shutdown(socket.SHUT_WR)  # FIN, not RST: let the client drain the answer
    conn.recv(4096)  # wait for the client's close
    conn.close()
