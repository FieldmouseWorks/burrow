"""A run GitHub queued and never started (no job, and it can't be cancelled)
leaves its head without CI: past UNSTARTED_AFTER it's dead, so CI is pending
on it and dead_runs names it to run again. A run that started is left alone,
and a cancelled check nothing ran through since is named by the same check."""

from docs import check, whole
from harness import C, clean
from prs import UNSTARTED_AFTER, ci_state, dead_runs

NOW = 1_800_000_000.0


def stamp(t):
    import datetime
    return datetime.datetime.fromtimestamp(t, datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def suite(id, age, jobs=0, status="QUEUED", run=77):
    return {"id": id, "status": status, "conclusion": None, "checkRuns": {"totalCount": jobs},
            "workflowRun": {"databaseId": run, "createdAt": stamp(NOW - age), "workflow": {"name": "flake"}}}


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
