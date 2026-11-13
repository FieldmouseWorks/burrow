"""A red default branch's undo: with the ledger on, a job that finds one PR's
merge broke it names that PR, and the steward has the ledger's writer cn
undo it. That undo's PR is the red's fix from then on, and only that one:
an undo the door or anyone else asked for lands as any PR does. The job
may name only a PR of the red repo; the ledger refusing it, finding nothing
to revert, or stopping partway again and again, leaves the red to its next
job, told why."""

import json
import os
import sys

from harness import C, FAIL, JOBS, R, STATES, WROTE, mkpr, stand_in, tmp
import tasks

stand_in()
C._for.append("sam")

# The ledger's writer cn: it records how it was run (its arguments, and
# which tokens it was handed), then prints what WRITER says, a row a line,
# and exits with its code.
WRITER, WRITER_LOG = f"{tmp}/writer.json", f"{tmp}/writer-log"
with open(f"{tmp}/writer-cn", "w") as f:
    f.write(f"""#!{sys.executable}
import json, os, sys
do = json.load(open({WRITER!r}))
with open({WRITER_LOG!r}, "a") as f:
    f.write(json.dumps({{"args": sys.argv[1:], "tokens": sorted(k for k in ("GH_TOKEN", "GITHUB_TOKEN", "CONARY_TOKEN_COMMAND")
                                                              if k in os.environ)}}) + "\\n")
for row in do.get("out", []):
    print(json.dumps(row) if isinstance(row, dict) else row)
print(do.get("err", ""), file=sys.stderr)
sys.exit(do.get("rc", 0))
""")
os.chmod(f"{tmp}/writer-cn", 0o755)


def writer(rc=0, out=(), err=""):
    with open(WRITER, "w") as f:
        json.dump({"rc": rc, "out": list(out), "err": err}, f)
    open(WRITER_LOG, "w").close()


def writer_runs():
    return [json.loads(x) for x in open(WRITER_LOG)]


def default(state, sha="c" * 40):
    return {"defaultBranchRef": {"name": "main", "target": {"oid": sha, "statusCheckRollup": {"state": state}}}}


def answer(jid, what):
    """What job jid left in its answers file."""
    os.makedirs(f"{tmp}/jobs/{jid}", exist_ok=True)
    with open(f"{tmp}/jobs/{jid}/answers.json", "w") as f:
        json.dump(what, f)


def undo_pr(number=40, **kw):
    pr = mkpr(branch=f"conary/undo/c{number}/1", number=number, **kw)
    pr["author"] = {"__typename": "Bot", "login": "workers"}
    return pr


def red():
    return C.load("repo-sam_u.json", {}).get("red") or {}


def start():
    """sam/u red at ccccccc, with no red seen before: its job, job-1, starts."""
    C.save("repo-sam_u.json", {})
    JOBS.clear()
    STATES["job-1"] = "running"
    STATES.pop("job-2", None)
    C.WAKE.clear()
    writer()
    assert R.watch_default(repo, default("FAILURE"), [], mine) == {}
    assert len(JOBS) == 1, JOBS


ROW = {"change": "c1", "pr": 40, "url": "https://github.com/sam/u/pull/40", "reverts": ["c12"], "left": [],
       "opened": True, "conflicts": False, "recorded": 2}
repo = {"repo": "sam/u", "private": False, "default": "main"}
mine = []
kept_undo = C.CFG.get("undo")

# A red's job is told: revert the one merge that broke it only to check,
# and name that PR in its answers file, of this repo, one only.
C.CFG["undo"] = f"{tmp}/writer-cn"
start()
assert '{"undo": "sam/u#N"}' in JOBS[0] and f"{tmp}/jobs/$PROJ_JOB/answers.json" in JOBS[0], JOBS[0]
assert "don't push the revert yourself" in JOBS[0] and "Name one PR, never more" in JOBS[0], JOBS[0]
# Its own checks come first, and its fallback is the fix it writes itself.
assert JOBS[0].index("Before anything else") < JOBS[0].index('{"undo"') < JOBS[0].index("make the smallest change"), JOBS[0]

# While it works, nothing is undone; once it ends, the PR it named is.
answer("job-1", {"undo": "sam/u#12"})
assert R.watch_default(repo, default("FAILURE"), [], mine) == {}
assert writer_runs() == [], writer_runs()
writer(out=[ROW])
STATES["job-1"] = "done"
assert R.watch_default(repo, default("FAILURE"), [], mine) == {}
# As the writer, with its own token: never the steward's, which would make
# its PR the steward App's, no fix of the machinery's.
assert writer_runs() == [{"args": ["undo", "sam/u#12"], "tokens": []}], writer_runs()
assert red()["undos"] == [40] and red()["asked"] and C.WAKE.get("sam/u") == C.NOW + 60, (red(), C.WAKE)
assert len(JOBS) == 1 and mine == [], (JOBS, mine)
# The undo's PR is the red's fix: nothing more is asked or started while
# it's open, and step.py holds it as one (tasks.red_undos).
writer(out=[ROW])
for _ in range(2):
    assert R.watch_default(repo, default("FAILURE"), [undo_pr()], mine) == {}
assert writer_runs() == [] and len(JOBS) == 1, (writer_runs(), JOBS)
assert tasks.red_undos("sam/u") == {40} and tasks.red_fix(undo_pr(), "main", {40})
# Anyone else's undo isn't: the door's (another number), one from a fork,
# a draft, or to another branch.
assert not tasks.red_fix(undo_pr(41), "main", {40})
assert not tasks.red_fix(dict(undo_pr(), isCrossRepository=True), "main", {40})
assert not tasks.red_fix(undo_pr(draft=True), "main", {40})
assert not tasks.red_fix(undo_pr(base="dev"), "main", {40})
# While CI on main runs, the red's undo waits for it like any fix, and the
# door's isn't held.
assert R.watch_default(repo, default("PENDING"), [undo_pr(), undo_pr(41)], mine) == {
    40: "Waiting for CI on main: it lands only while main is red"}
# Green again without it, it's closed as not needed; the door's isn't.
WROTE.clear()
assert R.watch_default(repo, default("SUCCESS", sha="e" * 40), [undo_pr(), undo_pr(41)], mine) == {
    40: "Not needed: main is green again without it"}
assert [x.split()[:3] for x in WROTE] == [["pr", "close", "40"]], WROTE
assert "green again at eeeeeee, so this undo isn't needed" in WROTE[0], WROTE
assert not red(), red()

# It may name only a PR of the red repo, as REPO#N: anything else is no
# undo, and the job ended with no fix, so the next is started, as ever.
for named in ("kim/x#12", "sam/uu#12", "sam/u#0", "sam/u#12 extra", "sam/u", 12, ["sam/u#12"], None):
    start()
    STATES["job-1"] = "done"
    answer("job-1", {"undo": named})
    R.watch_default(repo, default("FAILURE"), [], mine)
    R.watch_default(repo, default("FAILURE"), [], mine)
    assert writer_runs() == [] and len(JOBS) == 2, (named, writer_runs(), JOBS)
    assert "Don't name one again" not in JOBS[1] and '{"undo": "sam/u#N"}' in JOBS[1], (named, JOBS[1])
    STATES.pop("job-2", None)
# Nor is an answers file that isn't one, or none at all.
for got in ("[1, 2]", "{", None):
    start()
    STATES["job-1"] = "done"
    if got is None:
        os.remove(f"{tmp}/jobs/job-1/answers.json")
    else:
        with open(f"{tmp}/jobs/job-1/answers.json", "w") as f:
            f.write(got)
    R.watch_default(repo, default("FAILURE"), [], mine)
    R.watch_default(repo, default("FAILURE"), [], mine)
    assert writer_runs() == [] and len(JOBS) == 2, (got, writer_runs(), JOBS)
# Its case needn't match the repo's.
start()
STATES["job-1"] = "done"
answer("job-1", {"undo": "Sam/U#12"})
writer(out=[ROW])
R.watch_default(repo, default("FAILURE"), [], mine)
assert writer_runs()[0]["args"] == ["undo", "sam/u#12"] and red()["undos"] == [40], (writer_runs(), red())

# The ledger refuses it (cn's 65: not landed, reverted already, a PR it
# keeps no change of), or finds nothing to revert: the next job is told so,
# and not to name one again.
for rc, out, said in ((65, [], "cn: no landed change for sam/u#12"),
                      (0, [{"gone": ["c12"]}], "trunk lacks what it brought already")):
    start()
    STATES["job-1"] = "done"
    answer("job-1", {"undo": "sam/u#12"})
    writer(rc=rc, out=out, err="cn: no landed change for sam/u#12" if rc else "")
    R.watch_default(repo, default("FAILURE"), [], mine)
    assert len(writer_runs()) == 1 and red().get("asked") and not red().get("undos"), (writer_runs(), red())
    assert len(JOBS) == 1, JOBS  # the second look first, as for any job that ended with no fix
    R.watch_default(repo, default("FAILURE"), [], mine)
    assert len(JOBS) == 2 and "Don't name one again" in JOBS[1] and said in JOBS[1], JOBS[1]
    assert '{"undo"' not in JOBS[1], JOBS[1]
    # What that job names isn't asked for: it was told not to.
    STATES["job-2"] = "done"
    answer("job-2", {"undo": "sam/u#12"})
    writer()
    R.watch_default(repo, default("FAILURE"), [], mine)
    R.watch_default(repo, default("FAILURE"), [], mine)
    assert writer_runs() == [] and mine and mine[0][0] == "The default branch is red", (writer_runs(), mine)
    mine.clear()
    STATES.pop("job-2")

# GitHub unavailable (75), or a stop partway (any other exit): asked again
# five minutes after it stopped, keeping any PR it opened and stopped
# before recording, which is a fix already; after UNDO_TRIES, the next job
# is told. A pass sooner than that (any event starts one) asks nothing and
# uses up no try: it books the pass for then.
# The retry's clock is the time now, not the pass's start: here, C.NOW
# moved on by however long this pass has run.
kept_now, ran = C.NOW, [0]
R.clock = lambda: C.NOW + ran[0]
start()
STATES["job-1"] = "done"
answer("job-1", {"undo": "sam/u#12"})
writer(rc=1, out=[{"unrecorded": "https://github.com/sam/u/pull/40", "pr": 40}], err="cn: GitHub said 502")
C.WAKE.clear()
assert R.watch_default(repo, default("FAILURE"), [], mine) == {}
assert red()["undos"] == [40] and not red().get("asked"), red()
assert C.WAKE.get("sam/u") == red()["retryAt"] == C.NOW + 300, (red(), C.WAKE)
for _ in range(3):
    C.WAKE.clear()
    assert R.watch_default(repo, default("FAILURE"), [undo_pr()], mine) == {}
    assert len(writer_runs()) == 1 and red()["undoTries"] == 1 and C.WAKE.get("sam/u") == red()["retryAt"], (red(), C.WAKE)
# Asked again then, with its PR open: cn finds and records it.
C.NOW = red()["retryAt"]
writer(out=[dict(ROW, opened=False)])
assert R.watch_default(repo, default("FAILURE"), [undo_pr()], mine) == {}
assert len(writer_runs()) == 1 and red()["undos"] == [40] and not red()["theirs"] and red()["asked"], (writer_runs(), red())
assert len(JOBS) == 1, JOBS
C.NOW = kept_now
start()
STATES["job-1"] = "done"
answer("job-1", {"undo": "sam/u#12"})
writer(rc=75, err="cn: GitHub is unavailable")
for n in range(1, R.UNDO_TRIES):
    assert R.watch_default(repo, default("FAILURE"), [], mine) == {}
    assert red()["undoTries"] == n and not red().get("asked") and len(JOBS) == 1, (red(), JOBS)
    C.NOW = red()["retryAt"]
R.watch_default(repo, default("FAILURE"), [], mine)
R.watch_default(repo, default("FAILURE"), [], mine)
assert len(writer_runs()) == R.UNDO_TRIES, writer_runs()
assert len(JOBS) == 2 and f"stopped {R.UNDO_TRIES} times" in JOBS[1] and "GitHub is unavailable" in JOBS[1], JOBS[1]
C.NOW = kept_now
# A pass that ran seven minutes before it asked: five minutes from when cn
# stopped, not from the pass's start, and checked against the time then.
start()
STATES["job-1"] = "done"
answer("job-1", {"undo": "sam/u#12"})
writer(rc=75, err="cn: GitHub is unavailable")
ran[0] = 420
R.watch_default(repo, default("FAILURE"), [], mine)
assert red()["retryAt"] == C.NOW + 420 + 300 and red()["undoTries"] == 1, red()
C.NOW += 420 + 60
ran[0] = 0
R.watch_default(repo, default("FAILURE"), [], mine)
assert len(writer_runs()) == 1 and red()["undoTries"] == 1, (writer_runs(), red())
C.NOW = kept_now
R.clock = __import__("time").time

# Run past its time, what it printed before then counts all the same.
start()
STATES["job-1"] = "done"
answer("job-1", {"undo": "sam/u#12"})
kept_run = R.subprocess.run


def timed_out(args, **kw):
    raise R.subprocess.TimeoutExpired(args, 600, output=(json.dumps(ROW) + "\n").encode())


R.subprocess.run = timed_out
try:
    assert R.watch_default(repo, default("FAILURE"), [], mine) == {}
finally:
    R.subprocess.run = kept_run
assert red()["undos"] == [40] and red()["undoTries"] == 1, red()
assert R.watch_default(repo, default("PENDING"), [undo_pr()], mine) == {40: "Waiting for CI on main: it lands only while main is red"}

# In a pass, cn gets only what's left of the pass's remote-call deadline:
# it mustn't run the unit past the time the pass keeps to save its progress.
start()
STATES["job-1"] = "done"
answer("job-1", {"undo": "sam/u#12"})
given = []


def cut(args, **kw):
    given.append(kw["timeout"])
    raise R.subprocess.TimeoutExpired(args, kw["timeout"], output=(json.dumps(ROW) + "\n").encode())


R.subprocess.run = cut
try:
    with C.pass_deadline(30):
        assert R.watch_default(repo, default("FAILURE"), [], mine) == {}
finally:
    R.subprocess.run = kept_run
assert len(given) == 1 and 0 < given[0] <= 30, given
assert red()["undos"] == [40] and red()["undoTries"] == 1, red()
assert R.watch_default(repo, default("PENDING"), [undo_pr()], mine) == {40: "Waiting for CI on main: it lands only while main is red"}

# An undo cn found open that this red didn't ask for (the door's, say) is
# that PR's undo in hand: no other job starts while it's open, but it's no
# fix of this red's, never held or closed for it.
start()
STATES["job-1"] = "done"
answer("job-1", {"undo": "sam/u#12"})
writer(out=[dict(ROW, opened=False)])
R.watch_default(repo, default("FAILURE"), [], mine)
assert red()["theirs"] == [40] and not red()["undos"] and red()["asked"], red()
for _ in range(2):
    assert R.watch_default(repo, default("FAILURE"), [undo_pr()], mine) == {}
assert len(JOBS) == 1, JOBS
assert R.watch_default(repo, default("PENDING"), [undo_pr()], mine) == {}
WROTE.clear()
for _ in range(2):  # the second look, as for any job's red
    assert R.watch_default(repo, default("SUCCESS"), [undo_pr()], mine) == {}
assert WROTE == [] and not red(), (WROTE, red())
# Closed without landing, still red: the next job starts, and may name it
# again, for an undo of its own.
start()
STATES["job-1"] = "done"
answer("job-1", {"undo": "sam/u#12"})
writer(out=[dict(ROW, opened=False)])
R.watch_default(repo, default("FAILURE"), [], mine)
R.watch_default(repo, default("FAILURE"), [], mine)
R.watch_default(repo, default("FAILURE"), [], mine)
assert len(JOBS) == 2 and '{"undo": "sam/u#N"}' in JOBS[1], JOBS

# A PR cn names in another repo isn't kept: only the red repo's are its fix.
start()
STATES["job-1"] = "done"
answer("job-1", {"undo": "sam/u#12"})
writer(out=[dict(ROW, url="https://github.com/kim/x/pull/40")])
R.watch_default(repo, default("FAILURE"), [], mine)
assert not red().get("undos") and "opened no undo of sam/u" in red().get("refused", ""), red()

# Green again by the time its job ends: nothing is undone.
start()
STATES["job-1"] = "done"
answer("job-1", {"undo": "sam/u#12"})
writer(out=[ROW])
R.watch_default(repo, default("SUCCESS"), [], mine)
assert writer_runs() == [], writer_runs()

FAIL.clear()
STATES.pop("job-1", None)
C.CFG["undo"] = kept_undo
C.save("repo-sam_u.json", {})
JOBS.clear()
C._for.pop()

print("undos: ok")
