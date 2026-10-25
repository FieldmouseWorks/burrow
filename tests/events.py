"""burrow-events, with systemctl stood in for: which deliveries and kicks
start whose pass, when, and as which unit.

  python3 -I tests/events.py modules/events/events.py
  nix build .#checks.x86_64-linux.events
"""

import hashlib
import hmac
import http.server
import json
import os
import sys
import tempfile
import threading
import types
import urllib.error
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(sys.argv[1])))
import events as E  # noqa: E402

tmp = tempfile.mkdtemp()

# A delivery counts only when GitHub signed it with the hook's secret.
body = json.dumps({"repository": {"full_name": "sam/r"}}).encode()
sig = "sha256=" + hmac.new(b"hook-secret", body, hashlib.sha256).hexdigest()
assert E.signed("hook-secret", body, sig) and not E.signed("other", body, sig) and not E.signed("hook-secret", body, None)

# A repo is a unit's instance as systemd-escape has it, so %I gives it back.
assert E.escape("sam/r") == "sam-r" and E.escape("sam/my-repo") == "sam-my\\x2drepo", E.escape("sam/my-repo")
assert E.escape(".a/b_c.d") == "\\x2ea-b_c.d", E.escape(".a/b_c.d")
h = {"events": ["push"], "unit": "autoland", "wake": None}
assert E.unit(h, "sam/r") == "autoland@sam-r.service" and E.unit(h, E.FULL) == "autoland.service"
print("units: ok")

# A burst of events is one pass, a few seconds on; one repo's passes are GAP
# apart; only a wake-up booked since its last pass started counts; and a
# full pass sees to every repo whose event has come in, GAP or not.
line = E.Line()
line.event("sam/r", 100)
line.event("sam/r", 102)
assert line.take(104) == ([], 1) and line.take(105) == (["sam/r"], None) and line.take(106) == ([], None)
line.event("sam/r", 110)
assert line.take(115) == ([], 10) and line.take(125)[0] == ["sam/r"]
line.wakes({"sam/r": 120, "sam/other": 200, "--bad": 130, "sam/q": "soon"})
assert line.due == {"sam/other": 200}, line.due
line.wakes({"sam/r": 160})
assert line.take(159) == ([], 1) and line.take(160)[0] == ["sam/r"]
line = E.Line()
line.event("sam/r", 100)
line.take(105)
line.event("sam/r", 106)
line.event("sam/q", 106)
line.event(E.FULL, 106 - E.DEBOUNCE)
line.wakes({"sam/s": 500})
assert line.take(111) == ([E.FULL], 389), line.due
assert line.due == {"sam/s": 500} and line.take(120) == ([], 380), line.due
# GitHub's names are case-blind: two spellings are one repo's pass.
line = E.Line()
line.event("Sam/R", 100)
line.event("sam/r", 101)
line.wakes({"SAM/r": 103})
assert line.take(106) == (["sam/r"], None), line.due
# An event between a take that found nothing and the wait after it isn't
# slept through.
line = E.Line()
assert line.take(0) == ([], None)
line.event("sam/r", 0)
waited = threading.Thread(target=line.wait, args=(None,), daemon=True)
waited.start()
waited.join(5)
assert not waited.is_alive(), "the wait slept through an event that came in before it"
print("line: ok")

# Deliveries go to the handlers that take their event, but a commit status
# not to the handler whose passes posted it; kicks name a handler, and a
# repo or none (a full pass).
handlers = {"autoland": {"events": ["push", "pull_request", "status"], "unit": "autoland", "wake": None,
                         "ownStatuses": ["review-settled"]},
            "inbox": {"events": ["issues", "pull_request", "status"], "unit": "proj-inbox", "wake": None}}
lines = {name: E.Line() for name in handlers}
hooks = http.server.HTTPServer(("127.0.0.1", 0), E.deliveries(handlers, lines, "hook-secret"))
kicks = http.server.HTTPServer(("127.0.0.1", 0), E.kicks(lines))
for srv in (hooks, kicks):
    threading.Thread(target=srv.serve_forever, daemon=True).start()


def post(srv, path, data=b"", headers=None):
    req = urllib.request.Request(f"http://127.0.0.1:{srv.server_port}{path}", data=data, method="POST", headers=headers or {})
    try:
        return urllib.request.urlopen(req).status
    except urllib.error.HTTPError as e:
        return e.code


def deliver(data, signature, event):
    return post(hooks, "/burrow/hook", data, {"X-Hub-Signature-256": signature, "X-GitHub-Event": event})


def due():
    return {name: sorted(line.due) for name, line in lines.items()}


assert deliver(body, "sha256=" + "0" * 64, "push") == 401 and due() == {"autoland": [], "inbox": []}
assert deliver(b"", sig, "push") == 401 and due() == {"autoland": [], "inbox": []}
assert deliver(body, sig, "ping") == 202 and due() == {"autoland": [], "inbox": []}
odd = json.dumps({"repository": {"full_name": "--help"}}).encode()
assert deliver(odd, "sha256=" + hmac.new(b"hook-secret", odd, hashlib.sha256).hexdigest(), "push") == 204
assert due() == {"autoland": [], "inbox": []}
assert deliver(body, sig, "push") == 202 and due() == {"autoland": ["sam/r"], "inbox": []}, due()
assert deliver(body, sig, "pull_request") == 202 and due() == {"autoland": ["sam/r"], "inbox": ["sam/r"]}, due()
lines["autoland"].due.clear()
lines["inbox"].due.clear()


def status(context):
    data = json.dumps({"repository": {"full_name": "sam/r"}, "context": context}).encode()
    return deliver(data, "sha256=" + hmac.new(b"hook-secret", data, hashlib.sha256).hexdigest(), "status")


# The steward's own status is no news to it; to another handler (a job
# awaiting the PR) it is.
assert status("review-settled") == 202 and due() == {"autoland": [], "inbox": ["sam/r"]}, due()
lines["inbox"].due.clear()
assert status("proof") == 202 and due() == {"autoland": ["sam/r"], "inbox": ["sam/r"]}, due()
lines["autoland"].due.clear()
lines["inbox"].due.clear()
assert post(kicks, "/kick/inbox/sam/q") == 202 and due() == {"autoland": [], "inbox": ["sam/q"]}, due()
assert post(kicks, "/kick/autoland") == 202 and due() == {"autoland": [E.FULL], "inbox": ["sam/q"]}, due()
for bad in ("/kick/nope/sam/r", "/kick/inbox/--help/x", "/kick/inbox/sam", "/kick", "/burrow/hook"):
    assert post(kicks, bad) == 404, bad
assert due() == {"autoland": [E.FULL], "inbox": ["sam/q"]}, due()
# The kick port takes no deliveries, however they're signed.
assert post(kicks, "/burrow/hook", body, {"X-Hub-Signature-256": sig, "X-GitHub-Event": "push"}) == 404
print("deliveries and kicks: ok")

# Each due pass starts its unit, the full one before the rest, and the wake
# file is read when the service starts and after each pass.
wake = f"{tmp}/wake.json"
with open(wake, "w") as f:
    json.dump({"sam/w": 0}, f)
h = {"events": [], "unit": "autoland", "wake": wake}
line = E.Line()
line.event(E.FULL, 0)
started = []
RUNNING = set()  # units already running when asked for
FAILING = set()  # units whose pass fails


def systemctl(cmd, **kw):
    if cmd[1] == "show":
        return types.SimpleNamespace(stdout="activating\n" if cmd[-1] in RUNNING else "inactive\n")
    started.append(cmd)
    with open(wake, "w") as f:
        json.dump({"sam/next": 1}, f)  # what the pass booked
    return types.SimpleNamespace(returncode=1 if cmd[-1] in FAILING else 0)


E.serve("autoland", h, line, run=systemctl, once=True)
assert started == [["systemctl", "start", "autoland.service"]], started
assert line.due == {"sam/next": 1}, line.due
E.serve("autoland", h, line, run=systemctl, once=True)
assert started[1:] == [["systemctl", "start", "autoland@sam-next.service"]], started
# A pass already running when its turn comes (from before a restart) began
# before it was due: asking again only waits for it, so it runs once more.
started.clear()
RUNNING.add("autoland.service")
line = E.Line()
line.event(E.FULL, 0)
E.serve("autoland", h, line, run=systemctl, once=True)
assert started == [["systemctl", "start", "autoland.service"]] * 2, started
RUNNING.clear()
# A pass that failed (killed at its timeout, say) runs again a minute later,
# then two, up to an hour, and from scratch once one goes through: nothing
# else may come for it.
FAILING.add("autoland.service")
line = E.Line()
line.event(E.FULL, 0)
before = E.time.time()
E.serve("autoland", h, line, run=systemctl, once=True)
assert before + 60 <= line.due[E.FULL] <= E.time.time() + 60, line.due
assert [line.done(E.FULL, False, 0) for _ in range(7)] == [120, 240, 480, 960, 1920, 3600, 3600]
FAILING.clear()
line.done(E.FULL, True, 0)
assert line.done(E.FULL, False, 0) == 60 and line.done("sam/r", False, 0) == 60
print("passes: ok")

# With no handlers the service still runs: exiting would be a restart every
# few seconds.
with open(f"{tmp}/events.json", "w") as f:
    json.dump({"port": 0, "kickPort": 0, "handlers": {}}, f)
os.makedirs(f"{tmp}/cred", exist_ok=True)
with open(f"{tmp}/cred/webhook", "w") as f:
    f.write("hook-secret\n")
os.environ.update(EVENTS_CONFIG=f"{tmp}/events.json", CREDENTIALS_DIRECTORY=f"{tmp}/cred")
running = threading.Thread(target=E.main, daemon=True)
running.start()
running.join(2)
assert running.is_alive(), "burrow-events exited with no handlers"
print("no handlers: ok")

# With no host, no deliveries: kicks only, and no secret to read.
with open(f"{tmp}/kicks-only.json", "w") as f:
    json.dump({"port": None, "kickPort": 0, "handlers": {}}, f)
os.environ.update(EVENTS_CONFIG=f"{tmp}/kicks-only.json", CREDENTIALS_DIRECTORY=f"{tmp}/none")
ports = []
E.listen, listen = (lambda port, h: ports.append(port)), E.listen
running = threading.Thread(target=E.main, daemon=True)
running.start()
running.join(2)
E.listen = listen
assert running.is_alive() and ports == [0], ports
print("kicks only: ok")
