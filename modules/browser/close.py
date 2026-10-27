"""Closes the browser the way its own window would: Browser.close over CDP,
then waits for it to exit. A SIGTERM ends Chromium without writing the cookies
it has held for under half a minute, so a login made just before a restart or
a reboot would be lost.

close.py PORT: the main process is $MAINPID, as systemd's ExecStop= gets it.
"""

import base64
import json
import os
import socket
import sys
import time
import urllib.request

port = int(sys.argv[1])
pid = os.environ["MAINPID"]

try:
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/version", timeout=5) as r:
        url = json.load(r)["webSocketDebuggerUrl"]
except OSError:
    sys.exit(0)  # Not answering: nothing to close gracefully.

s = socket.create_connection(("127.0.0.1", port), timeout=5)
path = url.split(f":{port}", 1)[1]
key = base64.b64encode(os.urandom(16)).decode()
s.sendall(
    (
        f"GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n"
        f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\nSec-WebSocket-Version: 13\r\n\r\n"
    ).encode()
)
head = b""
while b"\r\n\r\n" not in head:
    chunk = s.recv(4096)
    if not chunk:
        break
    head += chunk
if not head.startswith(b"HTTP/1.1 101"):
    sys.exit(f"CDP refused the WebSocket: {head.splitlines()[0]!r}")

# One masked text frame, as a client must send.
msg = json.dumps({"id": 1, "method": "Browser.close"}).encode()
mask = os.urandom(4)
s.sendall(bytes([0x81, 0x80 | len(msg)]) + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(msg)))

for _ in range(200):
    if not os.path.exists(f"/proc/{pid}"):
        sys.exit(0)
    time.sleep(0.1)
sys.exit("the browser was still up 20 seconds after Browser.close")
