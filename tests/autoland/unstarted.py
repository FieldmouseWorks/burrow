"""A run GitHub queued and never started (no job, and it can't be cancelled)
leaves its head without CI: past UNSTARTED_AFTER it's dead, so CI is pending
on it and dead_runs names it to run again. A run that started is left alone,
and a cancelled check nothing ran through since is named by the same check."""

from docs import check, whole
from harness import ANNOTATIONS, C, T0, clean
from prs import UNSTARTED_AFTER, ci_state, dead_runs as _dead_runs

NOW = C.ts(T0) + 700  # where the step's pass (docs.at) puts the clock


def stamp(t):
    import datetime
    return datetime.datetime.fromtimestamp(t, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def suite(id, age, jobs=0, status="QUEUED", run=77):
    return {"id": id, "status": status, "conclusion": None, "checkRuns": {"totalCount": jobs},
            "workflowRun": {"databaseId": run, "createdAt": stamp(NOW - age), "workflow": {"name": "flake"}}}


def dead_runs(pr, repo, now=None):
    with C.acting_for(repo):
        return _dead_runs(pr, repo, now)


def head(suites=(), checks=()):
    return clean(checks=list(checks), suites=list(suites))


C.NOW = NOW
old = UNSTARTED_AFTER + 60
# Zero jobs for longer than the limit: named, and CI is pending on it, not absent.
dead = head([suite("s1", old)])
assert dead_runs(dead, "sam/r") == [77], dead_runs(dead, "sam/r")
state, names, _ = ci_state(dead, repo="sam/r")
assert state == "pending" and names == ["flake (never started, to be run again)"], (state, names)
# Younger than the limit: GitHub may just be slow.
assert dead_runs(head([suite("s1", UNSTARTED_AFTER - 60)]), "sam/r") == []
# Started: it has a job, even one waiting for a runner, or it's under way.
assert dead_runs(head([suite("s1", old, jobs=3)]), "sam/r") == []
assert dead_runs(head([suite("s1", old, status="IN_PROGRESS")]), "sam/r") == []
assert dead_runs(head([suite("s1", old, status="COMPLETED")]), "sam/r") == []
# A cancelled check with nothing after it is named by its run too.
cancelled = whole(suite="s2", conclusion="CANCELLED")
for c in cancelled:
    c["checkSuite"]["workflowRun"]["databaseId"] = 88
assert dead_runs(head(checks=cancelled), "sam/r") == [88]
assert dead_runs(head(checks=cancelled + whole(suite="s3")), "sam/r") == []

# A cancelled check whose run also has a failed or timed-out job isn't run
# again: that verdict is the head's. Nor while a job of it is still stopping.
def sibling(conclusion="FAILURE", status="COMPLETED", suite_status="COMPLETED"):
    run = whole(suite="s2", conclusion="CANCELLED")
    run[1]["checkSuite"]["status"] = suite_status
    extra = whole(suite="s2")[1]
    extra.update(name="check / other", conclusion=conclusion, status=status, databaseId=5)
    run.append(extra)
    for c in run:
        c["checkSuite"]["workflowRun"]["databaseId"] = 99
    return run
assert dead_runs(head(checks=sibling("FAILURE")), "sam/r") == []
assert dead_runs(head(checks=sibling("SUCCESS")), "sam/r") == [99]
assert dead_runs(head(checks=sibling("CANCELLED", "COMPLETED")), "sam/r") == [99]
assert dead_runs(head(checks=sibling(None, "IN_PROGRESS")), "sam/r") == []
assert dead_runs(head(checks=sibling("SUCCESS", suite_status="IN_PROGRESS")), "sam/r") == []
ANNOTATIONS[5] = ["The job running on runner x has exceeded the maximum execution time of 5 minutes."]
assert dead_runs(head(checks=sibling("CANCELLED")), "sam/r") == []
ANNOTATIONS.clear()

# A replacement run of the same workflow that started stands in for the idle one;
# an unrelated workflow, or one that judged another diff, doesn't.
def other(id, workflow="flake", status="COMPLETED", jobs=3):
    s = suite(id, 60, jobs=jobs, status=status, run=78)
    s["workflowRun"]["workflow"] = {"id": workflow, "name": workflow}
    return s
idle = suite("s1", old)
idle["workflowRun"]["workflow"] = {"id": "flake", "name": "flake"}
for status in ("COMPLETED", "IN_PROGRESS"):
    gone = head([idle, other("s9", status=status)])
    assert dead_runs(gone, "sam/r") == [], status
assert dead_runs(head([idle, other("s9", workflow="docs")]), "sam/r") == [77]
assert dead_runs(head([idle, other("s9", jobs=0, status="QUEUED")]), "sam/r") == [77]
# One that judged another base (stale) doesn't stand in.
elsewhere = whole(base="dev", suite="s9")
assert dead_runs(head([idle, other("s9")], checks=elsewhere), "sam/r") == [77]

# The step's ask, on the PR's head: once per run, and not counted when GitHub
# didn't answer or in status mode.
from docs import at
from harness import FAIL, WROTE


def asked(pr):
    at(pr)
    return [w for w in WROTE if w.endswith("/rerun")]


RERUN = "api -X POST repos/sam/r/actions/runs/77/rerun"
assert asked(head([suite("s1", old)])) == [RERUN], WROTE
assert asked(head(checks=whole(suite="s9"))) == []
FAIL.append("/rerun")
assert asked(head([suite("s1", old)])) == [RERUN]  # refused: said, not raised
FAIL.clear()
