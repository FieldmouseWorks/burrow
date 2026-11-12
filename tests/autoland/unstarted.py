"""A run GitHub queued and never started (no job, and it can't be cancelled)
leaves its head without CI: past UNSTARTED_AFTER it's dead, so CI is pending
on it and dead_runs names it to run again. A run that started is left alone,
and a cancelled check nothing ran through since is named by the same check."""

from docs import whole
from harness import ANNOTATIONS, C, T0, clean
from prs import FLAKE, UNSTARTED_AFTER, ci_state, dead_runs as _dead_runs

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

# A replacement run of the same workflow that did the work stands in for the
# idle one: under way with a job, or finished with a check run that ran. A
# skipped caller, an empty suite, an unrelated workflow, or a run that judged
# another diff doesn't.
def other(id, workflow="flake", status="COMPLETED", jobs=3, path=None):
    s = suite(id, 60, jobs=jobs, status=status, run=78)
    s["workflowRun"]["workflow"] = {"id": workflow, "name": workflow}
    if path:
        s["workflowRun"]["file"] = {"path": path}
    return s


def did(id, workflow="flake", conclusion="SUCCESS", path=None, lane=False):
    """The check runs a suite of `workflow` left: the work, and a lane job."""
    runs = whole(suite=id, conclusion=conclusion) if lane else [check("x / x", conclusion, suite=id)]
    for c in runs:
        if conclusion == "SKIPPED":
            c["conclusion"] = "SKIPPED"  # a skipped caller skips every job
        c["checkSuite"]["workflowRun"] = {"databaseId": 78, "workflow": {"id": workflow, "name": workflow},
                                          **({"file": {"path": path}} if path else {})}
    return runs


from docs import check
idle = suite("s1", old)
idle["workflowRun"]["workflow"] = {"id": "flake", "name": "flake"}
assert dead_runs(head([idle, other("s9", status="IN_PROGRESS")], did("s9")), "sam/r") == []
assert dead_runs(head([idle, other("s9")], did("s9")), "sam/r") == []
assert dead_runs(head([idle, other("s9")], did("s9", conclusion="SKIPPED")), "sam/r") == [77]  # a skipped caller
assert dead_runs(head([idle, other("s9", jobs=0)], []), "sam/r") == [77]  # an empty completed suite
assert dead_runs(head([idle, other("s9", workflow="docs")], did("s9", workflow="docs")), "sam/r") == [77]
assert dead_runs(head([idle, other("s9", jobs=0, status="QUEUED")]), "sam/r") == [77]
# One that judged another base (stale) doesn't stand in.
elsewhere = whole(base="dev", suite="s9")
assert dead_runs(head([idle, other("s9")], checks=elsewhere), "sam/r") == [77]
# The shared CI's caller and its twin for a new base are one workflow; a run of
# it that has no lane (a push's, a dispatch's) judged no PR.
caller = suite("s1", old)
caller["workflowRun"].update(workflow={"id": "A", "name": "flake"}, file={"path": FLAKE[0]})
twin = lambda **kw: other("s9", workflow="B", path=FLAKE[1], **kw)
assert dead_runs(head([caller, twin()], did("s9", workflow="B", path=FLAKE[1], lane=True)), "sam/r") == []
assert dead_runs(head([caller, twin()], did("s9", workflow="B", path=FLAKE[1])), "sam/r") == [77]
assert dead_runs(head([caller, twin()], did("s9", workflow="B", path=FLAKE[1], lane=True, conclusion="SKIPPED")), "sam/r") == [77]

# A rollup or suite list cut off at 100 can't clear a rerun: a failed sibling
# may be past it.
cut = head(checks=cancelled)
cut["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["pageInfo"] = {"hasNextPage": True}
assert dead_runs(cut, "sam/r") == []
cut = head([suite("s1", old)])
cut["commits"]["nodes"][0]["commit"]["checkSuites"]["pageInfo"] = {"hasNextPage": True}
assert dead_runs(cut, "sam/r") == []

# Many cancelled jobs of one run: its annotations are read once per job, not
# once per cancelled job per job.
many = whole(suite="s2", conclusion="CANCELLED")
for i in range(40):
    extra = whole(suite="s2")[1]
    extra.update(name=f"check / j{i}", conclusion="CANCELLED", databaseId=1000 + i)
    many.append(extra)
for c in many:
    c["checkSuite"]["workflowRun"]["databaseId"] = 66
reads = []
with C.acting_for("sam/r"):
    real = C.gh
    C.gh = lambda *a, **k: (reads.append(a), real(*a, **k))[1]
    try:
        assert _dead_runs(head(checks=many), "sam/r") == [66]
    finally:
        C.gh = real
ids = [a[3] for a in reads if "annotations" in a[3]]
assert len(ids) == len(set(ids)) <= 41, len(ids)

# The step's ask, on the PR's head: once per run, and not counted when GitHub
# didn't answer or in status mode.
from docs import at
from harness import ATTEMPTS, FAIL, WROTE


def asked(pr):
    at(pr)
    return [w for w in WROTE if w.endswith("/rerun")]


RERUN = "api -X POST repos/sam/r/actions/runs/77/rerun"
assert asked(head([suite("s1", old)])) == [RERUN], WROTE
assert asked(head(checks=whole(suite="s9"))) == []
FAIL.append("/rerun")
assert asked(head([suite("s1", old)])) == [RERUN]  # refused: said, not raised
FAIL.clear()

# A request GitHub took without our hearing (it didn't answer), whose attempt
# then finished cancelled again: the run's attempt number moved, so it isn't
# asked a second time. One it didn't take is asked again.
from harness import SILENT, run as one_pass, fresh_pr_state
lost = head([suite("s1", old)])
fresh_pr_state()
SILENT.append("/rerun")
one_pass(lost, 700)
assert [w for w in WROTE if w.endswith("/rerun")] == [RERUN]
SILENT.clear()
one_pass(lost, 760)  # not taken: attempt still 1
assert [w for w in WROTE if w.endswith("/rerun")] == [RERUN]
one_pass(lost, 820)  # taken: counted, so no more
assert [w for w in WROTE if w.endswith("/rerun")] == []
fresh_pr_state()
SILENT.append("/rerun")
one_pass(lost, 700)
SILENT.clear()
ATTEMPTS[77] = 2  # the lost ask was taken and has finished
one_pass(lost, 760)
assert [w for w in WROTE if w.endswith("/rerun")] == [], WROTE
ATTEMPTS.clear()
