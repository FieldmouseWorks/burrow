"""burrow-events: what happens on GitHub, or on this machine, starts the work
it calls for within seconds (../events.nix runs it). Nothing waits for a
timer to come round.

Each handler (the steward, proj-watch, proj-inbox) names the GitHub events it
cares about and a systemd unit: UNIT.service is its full pass, and
UNIT@OWNER-REPO.service its pass over one repo. Three things put a repo in a
handler's line:

  - A webhook delivery from GitHub (the steward keeps each repo's hook) whose
    X-GitHub-Event is one of the handler's. Only a delivery signed with the
    hook's secret counts, and all it says is which repo changed: the pass
    reads the rest from GitHub.
  - A kick on the local port: POST /kick/HANDLER/OWNER/REPO for one repo, or
    /kick/HANDLER for a full pass. Something on this machine that just
    changed what a pass would find kicks it: a proj job's turn ending, a
    DeepSeek review coming back. Kicks only start a pass, which reads
    everything for itself, so the port asks for no secret; it listens on
    127.0.0.1 and nginx never passes anything to it.
  - A wake-up the handler's last pass booked in its wake file, {repo: epoch},
    for when a wait it saw ends (a new head's settle delay, a reviewer's
    timeout, a reply old enough to hand on). Read after each pass the
    service starts, and when it starts.

A repo's pass starts DEBOUNCE seconds after its first event, so a push and
the events that follow it get one pass, and one repo's passes are GAP apart.
A handler's passes run one at a time. When the service starts, every handler
gets a full pass, which catches up on whatever happened while it was down.
"""

import hashlib
import hmac
import http.server
import json
import os
import re
import subprocess
import sys
import threading
import time

DEBOUNCE = 5
GAP = 20
MAX_BODY = 25 * 1024 * 1024  # GitHub's cap on a delivery
# OWNER/REPO as GitHub has them, and never one that reads as an option.
REPO_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9-]*/[A-Za-z0-9_.][A-Za-z0-9_.-]*$")
FULL = ""  # a full pass's place in a line


def log(msg):
    print(msg, file=sys.stderr, flush=True)


def signed(secret, body, header):
    """Whether GitHub signed this body with the secret (X-Hub-Signature-256)."""
    want = "sha256=" + hmac.new(secret.encode(), body, hashlib.sha256).hexdigest()
    return hmac.compare_digest(want, header or "")


def escape(s):
    """systemd-escape: a repo as a unit's instance name (%I gives it back)."""
    out = []
    for i, ch in enumerate(s):
        if ch == "/":
            out.append("-")
        elif ch.isascii() and (ch.isalnum() or ch in ":_.") and not (i == 0 and ch == "."):
            out.append(ch)
        else:
            out.append("".join(f"\\x{b:02x}" for b in ch.encode()))
    return "".join(out)


def unit(h, repo):
    return f"{h['unit']}.service" if repo == FULL else f"{h['unit']}@{escape(repo)}.service"


class Line:
    """One handler's repos due a pass, and when each one's last pass started."""

    def __init__(self):
        self.due, self.last = {}, {}
        self.cond = threading.Condition()

    def event(self, repo, now):
        with self.cond:
            self.due[repo] = min(self.due.get(repo, now + DEBOUNCE), now + DEBOUNCE)
            self.cond.notify()

    def wakes(self, booked):
        """The wake file: only a wake-up booked after the repo's last pass
        started is new (that pass rewrote its repo's)."""
        with self.cond:
            for repo, at in booked.items():
                if isinstance(repo, str) and REPO_RE.match(repo) and isinstance(at, (int, float)) and at > self.last.get(repo, 0):
                    self.due[repo] = min(self.due.get(repo, at), at)
            self.cond.notify()

    def take(self, now):
        """The repos whose pass starts now, the full pass first, and how long
        until the next one is due (None: nothing is)."""
        with self.cond:
            ready = sorted(r for r, at in self.due.items() if max(at, self.last.get(r, 0) + GAP) <= now)
            if FULL in ready:
                # It sees to every repo whose event has come in as well, GAP
                # or not.
                for r in [r for r, at in self.due.items() if r != FULL and at <= now]:
                    del self.due[r]
                    self.last[r] = now
                ready = [FULL]
            for r in ready:
                del self.due[r]
                self.last[r] = now
            later = [max(at, self.last.get(r, 0) + GAP) for r, at in self.due.items()]
            return ready, (min(later) - now if later else None)

    def wait(self, timeout):
        with self.cond:
            self.cond.wait(timeout)


def booked(path):
    try:
        with open(path) as f:
            got = json.load(f)
        return got if isinstance(got, dict) else {}
    except (OSError, ValueError):
        return {}


def serve(name, h, line, run=subprocess.run, once=False):
    """Start each of the handler's due passes, for as long as the service runs."""
    if h.get("wake"):
        line.wakes(booked(h["wake"]))
    while True:
        ready, wait = line.take(time.time())
        for repo in ready:
            log(f"{name}: {repo or 'full pass'}")
            run(["systemctl", "start", unit(h, repo)])
        if ready and h.get("wake"):
            line.wakes(booked(h["wake"]))
        if once:
            return
        if not ready:
            line.wait(wait)


def answer(req, code):
    req.send_response(code)
    req.send_header("Content-Length", "0")
    req.end_headers()


def body(req):
    """The request's body, or None once it has been answered for being too
    big or having no length."""
    try:
        n = int(req.headers.get("Content-Length") or 0)
    except ValueError:
        n = -1
    if not 0 <= n <= MAX_BODY:
        answer(req, 413 if n > MAX_BODY else 400)
        return None
    return req.rfile.read(n)


class Quiet(http.server.BaseHTTPRequestHandler):
    timeout = 30

    def log_message(self, fmt, *args):
        pass  # a line per delivery says nothing the passes' lines don't


def deliveries(handlers, lines, secret):
    """The webhook's deliveries, nginx passes them on: each goes to the
    handlers that take its event."""

    class Delivery(Quiet):
        def do_POST(self):
            got = body(self)
            if got is None:
                return
            if not got or not signed(secret, got, self.headers.get("X-Hub-Signature-256")):
                return answer(self, 401)
            try:
                repo = json.loads(got)["repository"]["full_name"]
            except (ValueError, KeyError, TypeError):
                return answer(self, 204)  # nothing about a repo
            if not isinstance(repo, str) or not REPO_RE.match(repo):
                return answer(self, 204)
            event = self.headers.get("X-GitHub-Event")
            for name, h in handlers.items():
                if event in h["events"]:
                    lines[name].event(repo, time.time())
            answer(self, 202)

    return Delivery


def kicks(lines):
    """POST /kick/HANDLER[/OWNER/REPO], from this machine."""

    class Kick(Quiet):
        def do_POST(self):
            if body(self) is None:
                return
            parts = self.path.split("?")[0].split("/")
            if len(parts) not in (3, 5) or parts[:2] != ["", "kick"] or parts[2] not in lines:
                return answer(self, 404)
            repo = "/".join(parts[3:]) or FULL
            if repo != FULL and not REPO_RE.match(repo):
                return answer(self, 404)
            lines[parts[2]].event(repo, time.time() - DEBOUNCE if repo == FULL else time.time())
            answer(self, 202)

    return Kick


def listen(port, h):
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", port), h)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


def main():
    cfg = json.load(open(os.environ["EVENTS_CONFIG"]))
    with open(os.path.join(os.environ.get("CREDENTIALS_DIRECTORY", ""), "webhook")) as f:
        secret = f.read().strip()
    handlers = cfg["handlers"]
    lines = {name: Line() for name in handlers}
    for line in lines.values():
        line.event(FULL, time.time() - DEBOUNCE)  # catching up
    listen(cfg["port"], deliveries(handlers, lines, secret))
    listen(cfg["kickPort"], kicks(lines))
    log(f"deliveries on 127.0.0.1:{cfg['port']}, kicks on 127.0.0.1:{cfg['kickPort']}: {', '.join(handlers)}")
    workers = [threading.Thread(target=serve, args=(name, h, lines[name]), daemon=True) for name, h in handlers.items()]
    for w in workers:
        w.start()
    for w in workers:
        w.join()


if __name__ == "__main__":
    main()
