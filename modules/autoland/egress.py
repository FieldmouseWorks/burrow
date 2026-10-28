"""A review agent's only way out of its sandbox, which has no network of its
own. `bridge`, the first thing run in there, listens on 127.0.0.1 and passes
each connection over a unix socket to the `Gate` the worker runs outside for
that one agent run. The gate lets through only what the agent needs: a
CONNECT to one of its API's hosts, on 443 (the agent's HTTPS_PROXY).
Anything else gets a 403, so a command a PR talks an agent into running
reaches nothing: not the machine's own services, not the LAN, not the web."""

import os
import socket
import subprocess
import sys
import threading

FORBIDDEN = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"


def dial(host):
    """A TCP connection to host:443."""
    return socket.create_connection((host, 443), timeout=60)


def pipe(a, b):
    """a's bytes to b until a ends, then b hears there's no more."""
    try:
        while data := a.recv(65536):
            b.sendall(data)
    except OSError:
        pass
    try:
        b.shutdown(socket.SHUT_WR)
    except OSError:
        pass


def splice(a, b, first=b""):
    """Both ways between a and b until both ends are done; `first` goes to b
    ahead of the rest."""
    if first:
        b.sendall(first)
    back = threading.Thread(target=pipe, args=(b, a), daemon=True)
    back.start()
    pipe(a, b)
    back.join()


class Gate:
    """The gate for one agent run, listening on the unix socket `path`.
    `hosts`: what it may CONNECT to."""

    def __init__(self, path, hosts):
        self.hosts = {h.lower() for h in hosts}
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.bind(path)
        self.sock.listen(64)
        threading.Thread(target=self.serve, daemon=True).start()

    def serve(self):
        while True:
            try:
                c, _ = self.sock.accept()
            except OSError:
                return
            threading.Thread(target=self.one, args=(c,), daemon=True).start()

    def close(self):
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.sock.close()

    def one(self, c):
        up = None
        try:
            head = b""
            while b"\r\n\r\n" not in head:
                data = c.recv(65536)
                if not data or len(head) > 65536:
                    return
                head += data
            head, rest = head.split(b"\r\n\r\n", 1)
            lines = head.decode("latin-1").split("\r\n")
            method, target = (lines[0].split(" ") + ["", ""])[:2]
            if method == "CONNECT":
                host, _, port = target.rpartition(":")
                if host.lower() not in self.hosts or port != "443":
                    c.sendall(FORBIDDEN)
                    return
                up = dial(host)
                c.sendall(b"HTTP/1.1 200 Connection established\r\n\r\n")
                splice(c, up, rest)
            else:
                c.sendall(FORBIDDEN)
        except OSError:
            pass
        finally:
            for s in (c, up):
                if s:
                    s.close()


def bridge(path, names, argv):
    """In the sandbox: listen on 127.0.0.1, hand each connection to the gate
    at `path`, and run argv with each of `names` in its environment set to
    the listener's URL. Exits as argv does."""
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(64)

    def one(c):
        u = socket.socket(socket.AF_UNIX)
        try:
            u.connect(path)
            splice(c, u)
        except OSError:
            pass
        finally:
            c.close()
            u.close()

    def serve():
        while True:
            c, _ = srv.accept()
            threading.Thread(target=one, args=(c,), daemon=True).start()

    threading.Thread(target=serve, daemon=True).start()
    url = f"http://127.0.0.1:{srv.getsockname()[1]}"
    env = {n: url for n in names}
    sys.exit(subprocess.run(argv, env={**os.environ, **env}).returncode)


if __name__ == "__main__":
    # egress.py SOCKET NAME,NAME -- argv...
    a = sys.argv[1:]
    bridge(a[0], [n for n in a[1].split(",") if n], a[a.index("--") + 1:])
