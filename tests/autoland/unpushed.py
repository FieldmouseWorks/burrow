"""A PR's fix jobs (claims.py), through burrow-upkeep's `claims` (the real
one, with a gh that answers as GitHub would and notes what it was asked, and
a proj that says how each job stands as the harness's does): their answers
go up only once the PR's head is the job's own push. A job whose push never landed says nothing, and the PR is handed on
as one that ended without its answer; one whose PR someone else pushed to
says nothing either, and is stopped while it still works. What goes up and
how is answer.rs's to check; this checks what the pass hands it and what it
makes of each outcome (LoK-web#695: a job's push was refused, and it said
"Fixed in" a commit GitHub never had)."""

import itertools
import json
import os
import sys

import harness
from harness import JOBS, STATES, STOPPED, T, UNSTOPPABLE, C, clean, fresh_pr_state, head, pr_state, stand_in, tmp, upkeep_config

stand_in()
kept_job, ids = C.job, itertools.count(1)


def job(repo, task, clone=False, since=None):
    """One id a job, across passes."""
    JOBS.append(task)
    return f"job-{next(ids)}"


def fresh():
    global ids
    fresh_pr_state()
    STATES.clear()
    STOPPED.clear()
    ids = itertools.count(1)


def run(*a, **kw):
    """A pass, its proj saying of each job what STATES does."""
    with open(PROJ_SAYS, "w") as f:
        json.dump(STATES, f)
    return harness.run(*a, **kw)


C.job = job
GH_CALLS, GH_SAYS, PROJ_SAYS = f"{tmp}/answer-gh-calls", f"{tmp}/answer-gh.json", f"{tmp}/answer-proj.json"
os.makedirs(f"{tmp}/answer-bin")
with open(f"{tmp}/answer-bin/proj", "w") as f:
    f.write(f"""#!{sys.executable}
import json, sys
print(json.load(open({PROJ_SAYS!r})).get(sys.argv[2], "running"))
""")
os.chmod(f"{tmp}/answer-bin/proj", 0o755)
kept_upkeep, kept_bot = C.CFG["upkeep"], C.CFG["machineryLogin"]
C.CFG["upkeep"] = upkeep_config(f"{tmp}/upkeep-answer.json", proj=f"{tmp}/answer-bin/proj")
with open(f"{tmp}/answer-bin/gh", "w") as f:
    f.write(f"""#!{sys.executable}
import json, sys
a = sys.argv[1:]
method, path = a[3], a[4]
body = json.loads(sys.stdin.read()) if "--input" in a else None
with open({GH_CALLS!r}, "a") as f:
    f.write(json.dumps([method, path, body]) + "\\n")
says = json.load(open({GH_SAYS!r}))
if path == "graphql":
    got = {{"data": {{"repository": {{"pullRequest": says["pr"]}}}}}} if "reviewThreads" in body["query"] else {{"data": {{}}}}
elif "/activity" in path:
    got = says["activity"]
elif method == "GET" and path == "repos/sam/r/pulls/5":
    pr = says["pr"]
    got = {{"state": pr["state"].lower(), "head": {{"sha": pr["headRefOid"]}}, "body": pr["body"]}}
elif method == "GET":
    got = []
else:
    got = {{}}
sys.stdout.write("HTTP/1.1 200 OK\\r\\n\\r\\n" + json.dumps(got))
""")
os.chmod(f"{tmp}/answer-bin/gh", 0o755)
kept_path, os.environ["PATH"] = os.environ["PATH"], f"{tmp}/answer-bin{os.pathsep}{os.environ['PATH']}"
THREAD = "PRRT_kwDOfound"
pushed, theirs = "b" * 40, "c" * 40


def github(at=head, pushes=()):
    """GitHub, from now on: the PR at `at`, its one review thread open, and
    the branch's pushes (who, to what head), newest first."""
    pr = {"state": "OPEN", "headRefOid": at, "headRefName": "feat", "body": "",
          "reviewThreads": {"pageInfo": {"hasNextPage": False},
                            "nodes": [{"id": THREAD, "isResolved": False, "comments": {"nodes": [{"body": "P1: off by one"}]}}]}}
    with open(GH_SAYS, "w") as f:
        json.dump({"pr": pr, "activity": [{"activity_type": "push", "after": h, "actor": {"login": who}} for who, h in pushes]}, f)
    open(GH_CALLS, "w").close()


def calls():
    return [json.loads(x) for x in open(GH_CALLS)]


def writes():
    """What it asked GitHub to change: a mutation, or a write of REST's."""
    return [c for c in calls() if c[0] != "GET" and not (c[1] == "graphql" and c[2]["query"].startswith("query"))]


def leaves(jid, said):
    """Job jid ends, its answers file saying `said`."""
    os.makedirs(f"{tmp}/jobs/{jid}", exist_ok=True)
    with open(f"{tmp}/jobs/{jid}/answers.json", "w") as f:
        json.dump(said, f)
    STATES[jid] = "done"


FIXED = {"pushed": pushed, "threads": [{"id": THREAD, "fixed": True, "reply": "Bounded the loop."}],
         "description": "Does it, bounded.", "comment": None}
open_thread = clean(threads=[False])

# A job whose push never landed (LoK-web#695's, refused): it says it fixed
# the thread in a commit the PR's head isn't, and the head is still the one
# it was handed. Nothing goes up: no reply, no resolve, no description.
fresh()
github()
run(open_thread, 0)
run(open_thread, 31 * 60)
assert len(JOBS) == 1 and pr_state()["answering"]["job-1"]["head"] == head, (JOBS, pr_state())
leaves("job-1", FIXED)
status, _ = run(open_thread, 40 * 60)
assert writes() == [] and any(c[1] == "graphql" for c in calls()), calls()
assert "job-1" not in pr_state()["answering"] and "its Claude job just ended" in status, (pr_state(), status)
# It ended without its answer: the next job is told why (and its status
# says so), and it isn't posted again.
status, _ = run(open_thread, 41 * 60)
assert len(JOBS) == 1 and f"its push of {pushed[:12]} never reached feat" in JOBS[0], JOBS
assert "a Claude job has it since 12:41am UTC; the last one's answers didn't go up" in status, status
assert "handedUnposted" not in pr_state(), pr_state()
# A second that does the same fails the PR for the owner, as any two jobs
# that end without fixing it do.
leaves("job-2", FIXED)
run(open_thread, 50 * 60)
run(open_thread, 51 * 60)
status, digest = run(open_thread, 52 * 60)
assert writes() == [] and "state=failure" in status, (writes(), status)
assert any(x[0] == "Two Claude jobs ended without fixing it" and "1 open review thread" in x[2] for x in digest), digest
print("unpushed answers: ok")

# Its push landed: its answers go up, on its commit, and the pass reads the
# PR again at once.
fresh()
github()
run(open_thread, 0)
run(open_thread, 31 * 60)
leaves("job-1", FIXED)
github(pushed, [("workers[bot]", pushed)])
moved = clean(sha=pushed, threads=[False])
status, _ = run(moved, 40 * 60)
said = [c[2]["variables"] for c in writes() if c[1] == "graphql"]
assert said[0] == {"t": THREAD, "b": f"**Claude job** · fixed in {pushed}\n\nBounded the loop."} and said[1] == {"t": THREAD}, said
assert ["PATCH", "repos/sam/r/pulls/5", {"body": "Does it, bounded."}] in writes(), writes()
assert "A Claude job's answers went up" in status and C.WAKE["sam/r"] == T + 40 * 60 + 1, (status, C.WAKE)
assert pr_state()["answering"] == {} and JOBS == [], (pr_state(), JOBS)
print("pushed answers: ok")

# Someone else pushed to the PR (its own session, as on #695) while the job
# worked: once GitHub says the head is their push, not the App's the jobs
# push as, the job is stopped, and nothing it says goes up.
C.CFG["machineryLogin"] = "workers[bot]"
fresh()
github()
run(open_thread, 0)
run(open_thread, 31 * 60)
github(theirs, [("sam", theirs)])
run(clean(sha=theirs, threads=[False]), 35 * 60)
assert STOPPED == ["job-1"] and pr_state()["answering"] == {} and writes() == [], (STOPPED, pr_state(), writes())
# Its own push isn't someone else's: it carries on, and so does one GitHub's
# record can't place yet, which is looked for again a few minutes on.
for pushes in ([("workers[bot]", pushed)], []):
    fresh()
    github()
    run(open_thread, 0)
    run(open_thread, 31 * 60)
    github(pushed, pushes)
    run(clean(sha=pushed, threads=[False]), 35 * 60)
    assert STOPPED == [] and "job-1" in pr_state()["answering"], (pushes, STOPPED, pr_state())
    assert bool(pushes) or C.WAKE["sam/r"] == T + 37 * 60, C.WAKE
# Without that App (jobs push as the user, as the owner's session may) a
# push can't be told apart: the job carries on, and what it leaves on a
# head that isn't its push stands down.
C.CFG["machineryLogin"] = None
fresh()
github()
run(open_thread, 0)
run(open_thread, 31 * 60)
github(theirs, [("sam", theirs)])
run(clean(sha=theirs, threads=[False]), 35 * 60)
assert STOPPED == [] and "job-1" in pr_state()["answering"], STOPPED
leaves("job-1", FIXED)
run(clean(sha=theirs, threads=[False]), 40 * 60)
assert writes() == [] and pr_state()["answering"] == {}, (writes(), pr_state())
# One queued for its slot when the head moves can't have pushed: stopped.
fresh()
github()
run(open_thread, 0)
run(open_thread, 31 * 60)
STATES["job-1"] = "queued"
run(clean(sha=theirs, threads=[False]), 35 * 60)
assert STOPPED == ["job-1"] and pr_state()["answering"] == {}, (STOPPED, pr_state())
# One proj won't stop is stopping.py's to try again, a strike each pass it
# still can't, as a new head's jobs are, until it goes.
fresh()
github()
run(open_thread, 0)
run(open_thread, 31 * 60)
STATES["job-1"] = "queued"
UNSTOPPABLE.add("job-1")
moved_on = clean(sha=theirs, threads=[False])
for i, at in enumerate((35, 37, 40)):
    run(moved_on, at * 60)
    assert pr_state()["stopping"] == ["job-1"] and pr_state()["strikes"] == {"stop": i + 1}, (i, pr_state())
    assert pr_state()["answering"] == {} and STOPPED == [], pr_state()
UNSTOPPABLE.clear()
run(moved_on, 45 * 60)
assert STOPPED == ["job-1"] and "stopping" not in pr_state() and "stop" not in pr_state()["strikes"], pr_state()
print("answers of a moved PR: ok")

fresh()
C.job = kept_job
os.environ["PATH"] = kept_path
C.CFG["upkeep"], C.CFG["machineryLogin"] = kept_upkeep, kept_bot
