"""A review agent's only way out of its sandbox, which has no network of its
own. `bridge`, the first thing run in there, listens on 127.0.0.1 and passes
each connection over a unix socket to the `Gate` the worker runs outside for
that one agent run. The gate lets through only what the agent needs:

- a CONNECT to one of its API's hosts, on 443 (the agent's HTTPS_PROXY);
- for an agent whose key the gate holds (DeepSeek), plain HTTP to the gate
  itself, sent on to its API over TLS with the key put in on the way: the key
  is never in the sandbox, so nothing in there can read it out or say it.

Anything else gets a 403, so a command a PR talks an agent into running
reaches nothing: not the machine's own services, not the LAN, not the web."""

import os
import select
import socket
import ssl
import subprocess
import sys
import threading

FORBIDDEN = b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
# Hop-by-hop headers, and the ones the gate sets itself on the API's requests.
HOP = {"host", "connection", "proxy-connection", "keep-alive", "proxy-authorization", "te", "upgrade"}


def dial(host):
    """A TCP connection to host:443."""
    return socket.create_connection((host, 443), timeout=60)


def secure(host):
    """A TLS connection to host:443, its certificate checked."""
    return ssl.create_default_context().wrap_socket(dial(host), server_hostname=host)


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


def relay(c, up, first):
    """splice() for a TLS `up`, on this one thread: a TLS socket read and
    written from two threads at once crashes the process. Over once `up`
    ends, or neither side says anything for a minute."""
    up.sendall(first)
    reading = True
    while True:
        ready = [up] if up.pending() else select.select([up, c] if reading else [up], [], [], 60)[0]
        if not ready:
            return
        if c in ready:
            if data := c.recv(65536):
                up.sendall(data)
            else:
                reading = False
        if up in ready:
            # What came in may be TLS's own (a session ticket), with nothing
            # to read yet: not waited on, so c isn't kept waiting meanwhile.
            up.settimeout(0)
            try:
                data = up.recv(65536)
            except ssl.SSLWantReadError:
                continue
            finally:
                up.settimeout(60)
            if not data:
                return
            c.sendall(data)


class Gate:
    """The gate for one agent run, listening on the unix socket `path`.
    `hosts`: what it may CONNECT to. `api`: (host, {header: value}) to send
    plain requests on to, those headers set to these values, or None."""

    def __init__(self, path, hosts, api=None):
        self.hosts, self.api = {h.lower() for h in hosts}, api
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
            elif self.api and target.startswith("/"):
                # One request a connection (Connection: close), so every
                # request on it is one the gate has seen and set the key on.
                host, set_ = self.api
                drop = HOP | {k.lower() for k in set_}
                keep = [x for x in lines[1:] if x.partition(":")[0].strip().lower() not in drop]
                out = [lines[0], f"Host: {host}", *(f"{k}: {v}" for k, v in set_.items()), "Connection: close", *keep]
                up = secure(host)
                relay(c, up, ("\r\n".join(out) + "\r\n\r\n").encode("latin-1") + rest)
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
    at `path`, and run argv with each of `names` ("NAME" or "NAME/path") in
    its environment set to the listener's URL (and that path). Exits as argv
    does."""
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
    env = {n.partition("/")[0]: url + n.partition("/")[1] + n.partition("/")[2] for n in names}
    sys.exit(subprocess.run(argv, env={**os.environ, **env}).returncode)


if __name__ == "__main__":
    # egress.py SOCKET NAME,NAME/path -- argv...
    a = sys.argv[1:]
    bridge(a[0], [n for n in a[1].split(",") if n], a[a.index("--") + 1:])
