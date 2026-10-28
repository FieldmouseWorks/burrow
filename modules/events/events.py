"""burrow-events: what happens on GitHub, or on this machine, starts the work
it calls for within seconds (../events.nix runs it). Nothing waits for a
timer to come round.

Each handler (the steward, proj-watch, proj-inbox, HOST-update, Renovate)
names the GitHub events it cares about and a systemd unit: UNIT.service is its
full pass, and UNIT@OWNER-REPO.service its pass over one repo. A handler with
no pass over one repo (`perRepo` off) gets its full pass for a repo's event.
Three things put a repo in a handler's line:

  - A webhook delivery from GitHub (the steward keeps each repo's hook) whose
    X-GitHub-Event is one of the handler's. Only a delivery signed with the
    hook's secret counts, and all it says is which repo changed: the pass
    reads the rest from GitHub. A handler can take only the repos it names (`repos`),
    only pushes to a repo's default branch (`pushes`), and only the other
    events about some branches (`branches`).
  - A kick on the local port: POST /kick/HANDLER/OWNER/REPO for one repo, or
    /kick/HANDLER for a full pass. Something on this machine that just
    changed what a pass would find kicks it: a proj job's turn ending, a
    review coming back. Kicks only start a pass, which reads
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

import fnmatch
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
RETRY = 60  # a pass that failed runs again this long after, doubling each time it fails again
RETRY_MAX = 3600


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


def covers(h, repo):
    """Whether the handler takes this repo: one of its `repos`, case-blind,
    or any repo when it names none."""
    names = h.get("repos")
    return names is None or repo.lower() in (n.lower() for n in names)


def branches(event, payload):
    """The branches a delivery other than a push is about: a pull request's
    head, the commit's whose checks or status it reports. None: it's about
    no branch (an issue, a comment)."""
    def get(*keys):
        v = payload
        for k in keys:
            v = v.get(k) if isinstance(v, dict) else None
        return v
    if event.startswith("pull_request"):
        names = [get("pull_request", "head", "ref")]
    elif event == "check_suite":
        names = [get("check_suite", "head_branch")]
    elif event == "check_run":
        names = [get("check_run", "check_suite", "head_branch")]
    elif event == "workflow_run":
        names = [get("workflow_run", "head_branch")]
    elif event == "status":
        names = [b.get("name") for b in get("branches") or [] if isinstance(b, dict)]
    else:
        return None
    return [n for n in names if isinstance(n, str)]


def takes(h, event, payload):
    """Whether this delivery (its repo already checked) starts the
    handler's pass."""
    if event not in h["events"] or not covers(h, payload["repository"]["full_name"]):
        return False
    if event == "status" and payload.get("context") in h.get("ownStatuses", ()):
        return False
    if event == "push":
        default = payload["repository"].get("default_branch")
        return h.get("pushes", "any") == "any" or (isinstance(default, str) and payload.get("ref") == "refs/heads/" + default)
    pats = h.get("branches")
    names = branches(event, payload) if pats is not None else None
    return names is None or any(fnmatch.fnmatchcase(n, p) for n in names for p in pats)


def place(h, repo):
    """Where a repo's event goes in the handler's line: the repo's own pass,
    or its full pass when it has none over one repo."""
    return repo if h.get("perRepo", True) else FULL


class Line:
    """One handler's repos due a pass, and when each one's last pass started.
    GitHub's names are case-blind, so a repo is kept in lower case: two
    spellings are one repo, and the handlers match it case-blind too."""

    def __init__(self):
        self.due, self.last = {}, {}
        self.fails = {}  # passes in a row that failed, by repo
        # Something came in since the last take: the wait after it returns
        # at once instead of sleeping through what it missed.
        self.news = False
        self.cond = threading.Condition()

    def event(self, repo, now):
        repo = repo.lower()
        with self.cond:
            self.due[repo] = min(self.due.get(repo, now + DEBOUNCE), now + DEBOUNCE)
            self.news = True
            self.cond.notify()

    def wakes(self, booked):
        """The wake file: only a wake-up booked after the repo's last pass
        started is new (that pass rewrote its repo's)."""
        with self.cond:
            for repo, at in booked.items():
                if not isinstance(repo, str) or not REPO_RE.match(repo):
                    continue
                repo = repo.lower()
                if isinstance(at, (int, float)) and at > self.last.get(repo, 0):
                    self.due[repo] = min(self.due.get(repo, at), at)
            self.news = True
            self.cond.notify()

    def take(self, now):
        """The repos whose pass starts now, the full pass first, and how long
        until the next one is due (None: nothing is)."""
        with self.cond:
            self.news = False
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

    def done(self, repo, ok, now):
        """A pass ended. One that failed (or never started) runs again a
        minute later, then two, up to an hour: no other event may come for
        it, and once one goes through its unit isn't failed any more."""
        if ok:
            self.fails.pop(repo, None)
            return None
        n = self.fails[repo] = self.fails.get(repo, 0) + 1
        at = now + min(RETRY * 2 ** (n - 1), RETRY_MAX)
        with self.cond:
            self.due[repo] = min(self.due.get(repo, at), at)
        return at

    def wait(self, timeout):
        with self.cond:
            if not self.news:
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
            u = unit(h, repo)
            log(f"{name}: {repo or 'full pass'}")
            # One already running (from before this service started, say)
            # began before this pass was due, and asking for it again only
            # waits for that one: so it runs again once that one is done.
            busy = run(["systemctl", "show", "-P", "ActiveState", u], capture_output=True, text=True).stdout.strip() == "activating"
            ok = run(["systemctl", "start", u]).returncode == 0
            if busy:
                ok = run(["systemctl", "start", u]).returncode == 0
            again = line.done(repo, ok, time.time())
            if again:
                log(f"{name}: {repo or 'full pass'} failed; again in {round(again - time.time())}s")
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
    handlers that take it (`takes`). A commit status goes to none whose own
    passes posted it (`ownStatuses`), which is no news to them."""

    class Delivery(Quiet):
        def do_POST(self):
            got = body(self)
            if got is None:
                return
            if not got or not signed(secret, got, self.headers.get("X-Hub-Signature-256")):
                # GitHub's secret and this one differ (a forgery, or a
                # secret changed and not yet restarted for): say so.
                log(f"a {self.headers.get('X-GitHub-Event')} delivery whose signature isn't the hook secret's; turned away")
                return answer(self, 401)
            try:
                payload = json.loads(got)
                repo = payload["repository"]["full_name"]
            except (ValueError, KeyError, TypeError):
                return answer(self, 204)  # nothing about a repo
            if not isinstance(repo, str) or not REPO_RE.match(repo):
                return answer(self, 204)
            event = self.headers.get("X-GitHub-Event") or ""
            for name, h in handlers.items():
                if takes(h, event, payload):
                    lines[name].event(place(h, repo), time.time())
            answer(self, 202)

    return Delivery


def kicks(handlers, lines):
    """POST /kick/HANDLER[/OWNER/REPO], from this machine. A repo the
    handler doesn't take (`repos`) is no kick."""

    class Kick(Quiet):
        def do_POST(self):
            if body(self) is None:
                return
            parts = self.path.split("?")[0].split("/")
            if len(parts) not in (3, 5) or parts[:2] != ["", "kick"] or parts[2] not in lines:
                return answer(self, 404)
            h, repo = handlers[parts[2]], "/".join(parts[3:]) or FULL
            if repo != FULL and not (REPO_RE.match(repo) and covers(h, repo)):
                return answer(self, 404)
            lines[parts[2]].event(place(h, repo), time.time() - DEBOUNCE if repo == FULL else time.time())
            answer(self, 202)

    return Kick


def listen(port, h):
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", port), h)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv


def main():
    cfg = json.load(open(os.environ["EVENTS_CONFIG"]))
    handlers = cfg["handlers"]
    lines = {name: Line() for name in handlers}
    for line in lines.values():
        line.event(FULL, time.time() - DEBOUNCE)  # catching up
    # GitHub's deliveries, where the machine takes them (a host and a secret).
    if cfg["port"] is not None:
        with open(os.path.join(os.environ.get("CREDENTIALS_DIRECTORY", ""), "webhook")) as f:
            secret = f.read().strip()
        listen(cfg["port"], deliveries(handlers, lines, secret))
        log(f"deliveries on 127.0.0.1:{cfg['port']}")
    listen(cfg["kickPort"], kicks(handlers, lines))
    log(f"kicks on 127.0.0.1:{cfg['kickPort']}: {', '.join(handlers)}")
    for name, h in handlers.items():
        threading.Thread(target=serve, args=(name, h, lines[name]), daemon=True).start()
    # The servers and the handlers' lines run until the service stops, with
    # no handlers too: exiting would be a restart every few seconds.
    threading.Event().wait()


if __name__ == "__main__":
    main()
