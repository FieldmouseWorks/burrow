"""The gate (egress.py) sending an agent's plain API request on over TLS,
with a local TLS server standing in for the API: the request goes through
with the key the gate holds, the answer comes back whole, and the TLS socket
is never read and written at once (from two threads, that crashed the
review service)."""

import socket
import ssl
import subprocess
import tempfile
import threading
import time

import egress as E

tmp = tempfile.mkdtemp()
cert, key = f"{tmp}/cert.pem", f"{tmp}/key.pem"
subprocess.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-keyout", key, "-out", cert,
                "-days", "1", "-subj", "/CN=api.example", "-addext", "subjectAltName=DNS:api.example"],
               check=True, capture_output=True)

srv = socket.socket()
srv.bind(("127.0.0.1", 0))
srv.listen(1)
tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
tls.load_cert_chain(cert, key)
seen = []


def api():
    s, _ = srv.accept()
    s = tls.wrap_socket(s, server_side=True)
    got = b""
    while b"\r\n\r\n" not in got or len(got.split(b"\r\n\r\n", 1)[1]) < 4:
        got += s.recv(65536)
    seen.append(got)
    time.sleep(0.3)  # the answer takes a while, as a model's does
    s.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhel")
    time.sleep(0.1)
    s.sendall(b"lo")
    s.close()


threading.Thread(target=api, daemon=True).start()
E.dial = lambda host: socket.create_connection(("127.0.0.1", srv.getsockname()[1]), timeout=60)

overlap, busy, lock = [], [0], threading.Lock()


class Watched:
    """The TLS socket to the API, noting any call made while another is under way."""

    def __init__(self, sock):
        self.sock = sock

    def __getattr__(self, name):
        f = getattr(self.sock, name)
        if name not in ("recv", "sendall", "send", "shutdown"):
            return f

        def call(*a):
            with lock:
                busy[0] += 1
                if busy[0] > 1:
                    overlap.append(name)
            try:
                return f(*a)
            finally:
                with lock:
                    busy[0] -= 1
        return call


# egress.secure, with this test's certificate the one it trusts.
E.secure = lambda host: Watched(ssl.create_default_context(cafile=cert).wrap_socket(E.dial(host), server_hostname=host))

g = E.Gate(f"{tmp}/gate", [], ("api.example", {"Authorization": "Bearer the-key"}))
c = socket.socket(socket.AF_UNIX)
c.connect(f"{tmp}/gate")
c.sendall(b"POST /chat HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer nothing\r\nContent-Length: 4\r\n\r\n")
time.sleep(0.2)  # the body comes after the head, as it can
c.sendall(b"body")
c.shutdown(socket.SHUT_WR)
back = b""
c.settimeout(10)
while data := c.recv(65536):
    back += data
g.close()

assert back == b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello", back
head, body = seen[0].split(b"\r\n\r\n", 1)
assert body == b"body" and b"Host: api.example" in head and b"Authorization: Bearer the-key" in head, seen
assert b"nothing" not in head, head
assert overlap == [], overlap
print("gate: ok")
