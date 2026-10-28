"""burrow-watchdog, with proj, GitHub (notify's gh) and burrow-events' kick
port stood in for: what it does with an item past its deadline, step by
step, and when it looks again.

  python3 -I tests/watchdog.py modules/watchdog/watchdog.py modules/notify/notify.sh
  nix build .#checks.x86_64-linux.watchdog
"""

import http.server
import json
import os
import subprocess
import sys
import tempfile
import threading
import time

tmp = tempfile.mkdtemp()
KICKS = []


class Kicks(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length") or 0))
        KICKS.append(self.path)
        self.send_response(202)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, *a):
        pass


port = http.server.HTTPServer(("127.0.0.1", 0), Kicks)
threading.Thread(target=port.serve_forever, daemon=True).start()

# proj: `run` records the job and prints its id (or fails, when told to);
# `state` answers from STATES. notify is the real one, each call logged; its
# gh keeps the issues in GITHUB, and fails every call while GitHub is down.
PROJ_LOG, STATES = f"{tmp}/proj-log", f"{tmp}/states.json"
NOTIFY_LOG, GITHUB, BIN = f"{tmp}/notify-log", f"{tmp}/github.json", f"{tmp}/bin"
os.makedirs(BIN)
user = subprocess.run(["id", "-un"], capture_output=True, text=True, check=True).stdout.strip()
notify_env = {"NOTIFY": "box-notify", "REPO": "sam/homelab", "ASSIGNEE": "", "LABEL": "updates",
              "DIR": f"{tmp}/notices", "BURROW_USER": user, "HOME_DIR": tmp}
os.makedirs(notify_env["DIR"])
for name, body in (("proj", f"""
import json, sys
a = sys.argv[1:]
open({PROJ_LOG!r}, "a").write(json.dumps(a) + "\\n")
states = json.load(open({STATES!r}))
if a[0] == "run":
    if states.get("refuse"):
        sys.exit(1)
    n = sum(1 for _ in open({PROJ_LOG!r}))
    print(f"job-{{n}}")
elif a[0] == "state":
    print(states.get(a[1], "done"))
"""), ("notify", f"""
import json, os, subprocess, sys
open({NOTIFY_LOG!r}, "a").write(json.dumps(sys.argv[1:]) + "\\n")
env = {{**os.environ, **{notify_env!r}, "PATH": {BIN!r} + ":" + os.environ["PATH"]}}
sys.exit(subprocess.run(["bash", "-euo", "pipefail", {sys.argv[2]!r}, *sys.argv[1:]], env=env).returncode)
"""), ("bin/gh", f"""
import json, sys
a = sys.argv[1:]
gh = json.load(open({GITHUB!r}))
if gh["down"]:
    sys.exit("gh: couldn't reach GitHub")
opt = dict(zip(a, a[1:]))
issues = gh["issues"]
if a[:2] == ["issue", "list"]:
    tag = opt["--search"].split('"')[1]
    print(next((i["number"] for i in issues if i["open"] and i["title"].startswith(tag)), ""))
elif a[:2] == ["issue", "create"]:
    issues.append({{"number": len(issues) + 1, "title": opt["--title"], "open": True, "comments": 0}})
elif a[:2] == ["issue", "comment"]:
    issues[int(a[2]) - 1]["comments"] += 1
elif a[:2] == ["issue", "close"]:
    issues[int(a[2]) - 1]["open"] = False
json.dump(gh, open({GITHUB!r}, "w"))
"""), ("bin/systemd-cat", """
import sys
sys.stdin.read()
""")):
    with open(f"{tmp}/{name}", "w") as f:
        f.write(f"#!{sys.executable}\n{body}")
    os.chmod(f"{tmp}/{name}", 0o755)

os.makedirs(f"{tmp}/state")
STEWARD, LINE = f"{tmp}/steward-watch.json", f"{tmp}/proj-watch.json"
with open(f"{tmp}/config.json", "w") as f:
    json.dump({
        "stateDir": f"{tmp}/state",
        "sources": {
            "autoland": {"file": STEWARD, "about": "the PR steward", "look": "journalctl -u 'autoland*'.",
                         "kick": "autoland", "stale": 86400},
            "proj": {"file": LINE, "about": "proj's line", "look": "proj status.", "kick": None, "stale": None},
        },
        "proj": f"{tmp}/proj", "project": "homelab", "jobTimeout": 60, "jobIdle": 20,
        "kick": f"http://127.0.0.1:{port.server_port}/kick",
        "escalateAfter": 7200, "ownerAfter": 14400, "stewardKick": "autoland",
        "labels": {"owner": "needs-owner", "hold": "hold"},
        "notify": f"{tmp}/notify", "owner": "Sam", "host": "box",
    }, f)
os.environ["WATCHDOG_CONFIG"] = f"{tmp}/config.json"
sys.path.insert(0, os.path.dirname(os.path.abspath(sys.argv[1])))
import watchdog as W  # noqa: E402

MIN = 60
T = 1_800_000_000.0  # the tests' clock


def write(path, items, at=None):
    with open(path, "w") as f:
        json.dump({"at": T if at is None else at, "items": items}, f)


def item(since, due, what="PR #7 in sam/r (\"fix it\")", phase="Settled; waiting for CI: build", kick="autoland/sam/r", **kw):
    return {"what": what, "link": "https://github.com/sam/r/pull/7", "phase": phase, "since": since, "due": due, "kick": kick, **kw}


def states(**s):
    with open(STATES, "w") as f:
        json.dump(s, f)


def lines(path):
    try:
        return [json.loads(x) for x in open(path)]
    except OSError:
        return []


def github(down=False, issues=None):
    """GitHub as notify's gh finds it: up or down, and its issues."""
    got = issues if issues is not None else json.load(open(GITHUB))["issues"]
    with open(GITHUB, "w") as f:
        json.dump({"down": down, "issues": got}, f)


def opened():
    """The owner's notices open on GitHub, by title."""
    return [i["title"] for i in json.load(open(GITHUB))["issues"] if i["open"]]


def notices():
    """What notify was asked to send: not its `pending` answers."""
    return [x for x in lines(NOTIFY_LOG) if x[0] != "pending"]


def reset():
    for p in (PROJ_LOG, NOTIFY_LOG, STEWARD, LINE, *(f"{tmp}/state/{n}" for n in os.listdir(f"{tmp}/state")),
              *(f"{notify_env['DIR']}/{n}" for n in os.listdir(notify_env["DIR"]))):
        if os.path.exists(p):
            os.remove(p)
    KICKS.clear()
    states()
    github(issues=[])


def run(now, fresh=True):
    """A pass at `now`. The steward writes its file on every pass of its own,
    so it's fresh then, unless the test is about one that isn't."""
    if fresh and os.path.exists(STEWARD):
        got = json.load(open(STEWARD))
        write(STEWARD, got["items"], at=now)
    W.run(now)
    return W.load("wake.json", {}).get(""), W.load("state.json", {}).get("stalls", {})


def escalated():
    """What the stuck issue is to carry (None: not written)."""
    try:
        return json.load(open(f"{tmp}/state/escalated.json"))
    except OSError:
        return None


def runs():
    return [a for a in lines(PROJ_LOG) if a[0] == "run"]


# Nothing to watch: nothing done, and a pass booked only for when the
# steward, which should write every so often, would be stale.
reset()
assert run(T) == (T + 86400, {}) and not KICKS and not runs()
# An item not yet due: left be, with a pass booked for its deadline. So is a
# source that should write every so often: a pass when it would be stale.
write(STEWARD, {"sam/r#7": item(T - 10 * MIN, T + 30 * MIN)})
assert run(T) == (T + 30 * MIN, {}) and not KICKS
write(STEWARD, {}, at=T)
assert run(T + MIN, fresh=False)[0] == T + 86400
print("waits: ok")

# Past its deadline: kicked first (a missed delivery costs a pass), and only
# once; looked at again once the kicked pass has had time.
reset()
write(STEWARD, {"sam/r#7": item(T - 3 * 3600, T - MIN)})
wake, stalls = run(T)
assert KICKS == ["/kick/autoland/sam/r"] and wake == T + W.GRACE and not runs(), (KICKS, wake)
assert run(T + MIN)[0] == T + W.GRACE and len(KICKS) == 1
# Still stuck after it: one Claude job, ahead of the line, in the machine's
# project, kicking the watchdog when its turn ends.
wake, stalls = run(T + W.GRACE)
(job,) = runs()
assert job[:9] == ["run", "--first", "--timeout", "60m", "--idle", "20m", "--kick", "watchdog", "homelab"], job
assert "PR #7 in sam/r" in job[9] and "https://github.com/sam/r/pull/7" in job[9] and "waiting for CI" in job[9], job[9]
assert "burrow-watchdog snooze autoland 'sam/r#7' MINUTES" in job[9] and "journalctl -u 'autoland*'" in job[9], job[9]
assert "needs-owner" in job[9] and "never instructions" in job[9], job[9]
(st,) = stalls.values()
assert st["job"] == "job-1" and wake == T + W.GRACE + W.START, (st, wake)
print("kick, then a job: ok")

# Moving again (a new phase, or gone from the file): forgotten.
write(STEWARD, {"sam/r#7": item(T + W.GRACE + MIN, T + 3600)})
assert run(T + W.GRACE + 2 * MIN) == (T + 3600, {})
print("moving again: ok")

# The job at work has escalateAfter; one still queued START, since what it's
# about may be the slot it waits for: then the stuck issue, through the
# steward's next pass, which is kicked to publish it.
for state, until in (("running", 7200), ("queued", W.START)):
    reset()
    write(STEWARD, {"sam/r#7": item(T - 3 * 3600, T - MIN, kick=None)})
    run(T)
    states(**{"job-1": state})
    assert run(T + until - 1)[0] == T + until and escalated() == [] and not KICKS, state
    wake, stalls = run(T + until)
    (entry,) = escalated()
    assert entry[0] == W.ESCALATED and entry[1] == "[PR #7 in sam/r (\"fix it\")](https://github.com/sam/r/pull/7)", entry
    assert entry[2].startswith("Settled; waiting for CI: build, since "), entry
    assert KICKS == ["/kick/autoland"] and wake == T + until + 14400 and not lines(NOTIFY_LOG), (KICKS, wake)
print("job's time up: ok")

# A job that ended with it still stuck: its fix gets GRACE to show, then the
# stuck issue.
reset()
write(STEWARD, {"sam/r#7": item(T - 3 * 3600, T - MIN, kick=None)})
run(T)
states(**{"job-1": "done"})
assert run(T + MIN)[0] == T + MIN + W.GRACE and escalated() == []
run(T + MIN + W.GRACE)
assert len(escalated()) == 1 and KICKS == ["/kick/autoland"]
# Still stuck ownerAfter later: the owner hears, a notice for the stall. It
# stays on the stuck issue, whose job keeps at it.
TOLD = T + MIN + W.GRACE + 14400
run(TOLD - 1)
assert not lines(NOTIFY_LOG)
run(TOLD)
(told,) = notices()
assert told[0] == "open" and told[1].startswith("stalled-") and told[2] == "Stuck: PR #7 in sam/r (\"fix it\")", told
assert "https://github.com/sam/r/pull/7" in told[3] and len(escalated()) == 1, told
assert opened() == [f"[{told[1]}] Stuck: PR #7 in sam/r (\"fix it\")"], opened()
# notify can't say it got through, so it's sent again every RENOTIFY while
# it lasts, by whatever pass comes first: one issue all the same.
run(TOLD + W.RENOTIFY - 1)
assert len(notices()) == 1
run(TOLD + W.RENOTIFY)
assert notices()[1] == told and len(opened()) == 1, (notices(), opened())
# Once it moves, the notice closes and nothing's left of the stall.
write(STEWARD, {})
MOVED = TOLD + 2 * W.RENOTIFY
assert run(MOVED)[1] == {} and notices()[2] == ["close", told[1], "It's moving again."]
assert opened() == [] and escalated() == [] and KICKS[-1] == "/kick/autoland", (opened(), KICKS)
assert W.load("state.json", {})["closing"] == {}
print("stuck issue, then the owner: ok")


def told_at():
    """A stall taken to the owner, in the fewest passes: when they heard."""
    write(STEWARD, {"sam/r#7": item(T - 3 * 3600, T - MIN, kick=None)})
    run(T)
    run(T + MIN)
    run(T + MIN + W.GRACE)
    run(T + MIN + W.GRACE + 14400)
    return T + MIN + W.GRACE + 14400


# GitHub down: a notice that didn't get through is sent again once it's
# back, and a close is sent on every pass until it has, days if need be.
reset()
github(down=True)
TOLD = told_at()
assert notices()[0][0] == "open" and opened() == []
github()
run(TOLD + W.RENOTIFY)
assert len(opened()) == 1
github(down=True)
write(STEWARD, {})
for days in range(4):
    run(T + 86400 * (days + 1))
    assert len(opened()) == 1 and len(W.load("state.json", {})["closing"]) == 1
github()
run(T + 86400 * 5)
assert opened() == [] and W.load("state.json", {})["closing"] == {}
closes = len(notices())
run(T + 86400 * 6)
assert len(notices()) == closes
print("GitHub down: ok")

# Back as it was before its close got through (gone from the file a while,
# its source stale, its deadline moved): it carries on from where it had got
# to, and its notice, still open, isn't closed under it.
reset()
told_at()
assert len(opened()) == 1
github(down=True)
write(STEWARD, {})
run(T + 6 * 3600)
github()
closes = sum(x[0] == "close" for x in notices())
write(STEWARD, {"sam/r#7": item(T - 3 * 3600, T - MIN, kick=None)})
wake, stalls = run(T + 6 * 3600 + MIN)
(st,) = stalls.values()
assert st.get("told") and len(escalated()) == 1 and not W.load("state.json", {})["closing"], st
run(T + 6 * 3600 + 2 * MIN)
assert len(opened()) == 1 and sum(x[0] == "close" for x in notices()) == closes == 1, notices()
print("back again: ok")

# Every stall ready in one pass goes to one job: often one cause.
reset()
write(STEWARD, {"sam/r#7": item(T - 3 * 3600, T - MIN, kick=None),
                "sam/q#2": item(T - 3 * 3600, T - MIN, what="PR #2 in sam/q", kick=None)})
write(LINE, {"homelab-1": item(T - 3600, T - MIN, what="proj job homelab-1", phase="waiting for a slot", kick=None)})
run(T)
(job,) = runs()
assert "PR #7" in job[9] and "PR #2" in job[9] and "proj job homelab-1" in job[9]
# The watchdog's own job waiting in proj's line is its own steps' to time.
write(LINE, {"job-1": item(T - 3600, T - MIN, what="proj job job-1", phase="waiting for a slot", kick=None)})
assert set(run(T + MIN)[1]) == {W.sid("autoland", "sam/r#7", T - 3 * 3600), W.sid("autoland", "sam/q#2", T - 3 * 3600)}
print("one job a pass: ok")

# A job that couldn't start is tried again RETRY later, and after
# escalateAfter it's the next step's.
reset()
states(refuse=True)
write(STEWARD, {"sam/r#7": item(T - 3 * 3600, T - MIN, kick=None)})
assert run(T)[0] == T + W.RETRY and len(runs()) == 1
run(T + W.RETRY - 1)
assert len(runs()) == 1
run(T + W.RETRY)
assert len(runs()) == 2 and not escalated()
run(T + 7200)
assert len(escalated()) == 1
print("no job: ok")

# Each look comes by the time its next step could, even with an escalateAfter
# shorter than START or RETRY: a queued job's, or one that couldn't start.
for refuse in (False, True):
    reset()
    W.CFG["escalateAfter"] = 60
    states(refuse=refuse)
    write(STEWARD, {"sam/r#7": item(T - 3 * 3600, T - MIN, kick=None)})
    assert run(T)[0] == T + 60, refuse
    states(refuse=refuse, **{"job-1": "queued"})
    run(T + 60)
    assert len(escalated()) == 1, refuse
# And a retry's deadline between two retries.
reset()
W.CFG["escalateAfter"] = W.RETRY + 60
states(refuse=True)
write(STEWARD, {"sam/r#7": item(T - 3 * 3600, T - MIN, kick=None)})
run(T)
assert run(T + W.RETRY)[0] == T + W.RETRY + 60 and len(runs()) == 2
# A pass for something else in between books it all the same.
assert run(T + W.RETRY + 30)[0] == T + W.RETRY + 60 and len(runs()) == 2
run(T + W.RETRY + 60)
assert len(escalated()) == 1
W.CFG["escalateAfter"] = 7200
print("looks in time: ok")

# A stale source is one stall, not one an item: its kick, then a job. When
# that's the steward, the owner hears in place of its stuck issue, which it
# couldn't publish.
reset()
W.CFG["sources"]["autoland"]["stale"] = 7200
write(STEWARD, {"sam/r#7": item(T - 3 * 3600, T - MIN), "sam/q#2": item(T - 3 * 3600, T - MIN)}, at=T - 7200)
wake, stalls = run(T, fresh=False)
(st,) = stalls.values()
assert st["key"] == "" and st["what"] == "the PR steward" and KICKS == ["/kick/autoland"], (st, KICKS)
assert "hasn't changed since" in st["phase"]
run(T + W.GRACE, fresh=False)
(job,) = runs()
assert "the PR steward" in job[9] and "burrow-watchdog snooze autoland - MINUTES" in job[9]
states(**{"job-1": "running"})
run(T + W.GRACE + 7200, fresh=False)
assert escalated() == [] and notices()[0][2] == "Stuck: the PR steward"
# A source that has never written its file goes stale from when that was
# first seen.
reset()
assert run(T)[0] == T + 7200
assert list(run(T + 7200)[1].values())[0]["phase"].startswith("it has written no watch file since")
W.CFG["sources"]["autoland"]["stale"] = 86400
print("stale source: ok")

# Without a steward, the step after the job is the owner.
reset()
W.CFG["stewardKick"] = None
write(LINE, {"homelab-1": item(T - 3600, T - MIN, what="proj job homelab-1", kick=None)})
run(T)
states(**{"job-1": "done"})
run(T + MIN)
run(T + MIN + W.GRACE)
assert len(notices()) == 1 and escalated() == [] and not KICKS
W.CFG["stewardKick"] = "autoland"
print("no steward: ok")

# A job that finds it legitimately under way gives it more time, once, up to
# SNOOZE_MAX; meanwhile no step is taken.
reset()
write(STEWARD, {"sam/r#7": item(T - 3 * 3600, T - MIN, kick=None)})
run(T)
W.snooze("autoland", "sam/r#7", 999, "its gate's queue is two hours deep", T + MIN)
states(**{"job-1": "done"})
assert run(T + 2 * MIN)[0] == T + MIN + W.SNOOZE_MAX * MIN and escalated() == []
try:
    W.snooze("autoland", "sam/r#7", 30, "again", T + 3 * MIN)
    raise AssertionError("snoozed twice")
except SystemExit as e:
    assert "once already" in str(e)
try:
    W.snooze("autoland", "sam/x#1", 30, "nothing", T)
    raise AssertionError("snoozed what isn't stalled")
except SystemExit as e:
    assert "nothing to snooze" in str(e)
# Its time up and still stuck: the next step, as its job has ended.
run(T + MIN + W.SNOOZE_MAX * MIN)
assert len(escalated()) == 1
print("snooze: ok")

# What a source writes that isn't an item is left out, not a crash.
reset()
write(STEWARD, {"a": "no", "b": {"what": "x", "phase": "y", "since": "1", "due": T - 1}, "": item(T - 9, T - 1),
                "c": item(T - 9, T - 1, kick=None)})
assert list(run(T)[1]) == [W.sid("autoland", "c", T - 9)]
with open(STEWARD, "w") as f:
    f.write("{half")
assert run(T, fresh=False)[1] == {}
print("bad files: ok")

# The command: status changes nothing; snooze takes the pass's lock.
reset()
now = time.time()  # the command's own clock
write(STEWARD, {"sam/r#7": item(now - 3 * 3600, now - MIN)}, at=now)
env = dict(os.environ)
out = subprocess.run([sys.executable, "-I", sys.argv[1], "status"], env=env, capture_output=True, text=True, check=True).stdout
assert "OVERDUE sam/r#7" in out and not os.path.exists(f"{tmp}/state/state.json") and not KICKS, out
bad = subprocess.run([sys.executable, "-I", sys.argv[1], "snooze", "autoland", "x", "soon", "why"], env=env, capture_output=True, text=True)
assert bad.returncode != 0 and "usage" in bad.stderr, bad.stderr
print("command: ok")
