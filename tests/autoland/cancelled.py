"""A cancelled check run: a PR's new run on the same head (closed and reopened
while CI ran, say) cancels its last one, whose suite stays on the head. It goes
by what its workflow did after it: another suite that has done that check's
work for the PR's diff clears it, one still going leaves CI pending, and with
neither it's pending until it's run again, as when a runner shut down under
it or someone cancelled it by hand: run again once, and red if cancelled again,
since a hang is the head's to look at."""

from docs import at, check, docs, whole
from harness import ANNOTATIONS, JOBS, RERUN_FAILS, RERUNS, RUN_INFO, C, clean, fresh_pr_state, run as one_pass
from prs import ci_state

CANCEL, CHECK = "CANCELLED", "check / check"


def stopped(*names):
    """CI pending on cancelled check runs nothing ran through since."""
    return "pending", [f"{n} (cancelled, to be run again)" for n in names]


def state(*checks, base="main"):
    with C.acting_for("sam/r"):
        return ci_state(clean(checks=[c for run in checks for c in run], base=base), repo="sam/r")[:2]


def going(suite):
    return [check("check / lane into main", suite=suite, done="IN_PROGRESS"),
            check(CHECK, None, "IN_PROGRESS", suite=suite, done="IN_PROGRESS")]


old = whole(suite="s1", conclusion=CANCEL)

# The newer run passed: green, in whichever order the rollup lists them, and it
# lands with no fix job.
assert state(old, whole(suite="s2")) == ("green", [])
assert state(whole(suite="s2"), old) == ("green", [])
status, landed = at(clean(checks=old + whole(suite="s2")))
assert landed and JOBS == [] and "CI is red" not in status, (status, JOBS)
# Its lane job cancelled instead, so its gate never ran.
lane = [check("check / lane into main", CANCEL, suite="s1"), check(CHECK, "SKIPPED", suite="s1")]
assert state(lane, whole(suite="s2")) == ("green", [])
# The newer run's own red is the head's.
assert state(old, whole(suite="s2", conclusion="FAILURE")) == ("red", [CHECK])

# It's still going: pending, not red, and no fix job.
assert state(old, going("s2"))[0] == "pending"
status, landed = at(clean(checks=old + going("s2")))
assert not landed and JOBS == [] and "waiting for CI" in status, (status, JOBS)
# Run after run, cancelled: the last one decides.
assert state(old, whole(suite="s2", conclusion=CANCEL), whole(suite="s3")) == ("green", [])
assert state(old, whole(suite="s2", conclusion=CANCEL), going("s3"))[0] == "pending"

# flake.yml and flake-retarget.yml are one workflow: a retarget's run clears the
# push's that it cancelled on the same head. Unrelated workflows that only share
# a name don't clear each other, in either file's place.
def renamed(run, path, id):
    for c in run:
        c["checkSuite"]["workflowRun"].update(file={"path": path}, workflow={"id": id, "name": "flake"})
    return run


RETARGET = ".github/workflows/flake-retarget.yml"
retargeted = renamed(whole(suite="s2"), RETARGET, "twin")
assert state(old, retargeted) == ("green", [])
assert state(retargeted, old) == ("green", [])
namesake = renamed(whole(suite="s2"), ".github/workflows/ci.yml", "other")
assert state(old, namesake) == stopped(CHECK)
assert state(namesake, old) == stopped(CHECK)
nameless = renamed(whole(suite="s2"), None, "other")
assert state(old, nameless) == stopped(CHECK)
# Across the two files, a run still going leaves CI pending, either way round,
# and a title edit's skipped run or another cancelled one clears nothing.
assert state(old, renamed(going("s2"), RETARGET, "twin"))[0] == "pending"
assert state(renamed(whole(suite="s1", conclusion=CANCEL), RETARGET, "twin"), going("s2"))[0] == "pending"
assert state(old, renamed([check(CHECK, "SKIPPED", suite="s2")], RETARGET, "twin")) == stopped(CHECK)
assert state(old, renamed(whole(suite="s2", conclusion=CANCEL), RETARGET, "twin"))[0] != "green"

# One GitHub stopped at its job's timeout-minutes, as its annotation says, is
# the head's: red, and a fix job's. A runner shut down under it, or a
# person's cancel, says something else, and is pending. GitHub not saying
# fails the step, to be tried again: neither way is guessed.
timeout = [{**c, "databaseId": 41} if c["name"] == CHECK else c for c in old]
ANNOTATIONS[41] = ["The job running on runner remi-3 has exceeded the maximum execution time of 90 minutes."]
assert state(timeout) == ("red", [f"{CHECK} (timed out)"])
status, landed = at(clean(checks=timeout))
assert not landed and "CI is red: check / check (timed out)" in status, status
# Past the first page, after a job's many warnings: still red.
ANNOTATIONS[41] = ["warning"] * 150 + ANNOTATIONS[41]
assert state(timeout) == ("red", [f"{CHECK} (timed out)"])
# A newer run that ran through still clears it, unasked.
ANNOTATIONS[41] = C.Unavailable("gh api check-runs: HTTP 502")
assert state(timeout, whole(suite="s2")) == ("green", [])
try:
    state(timeout)
except C.Unavailable:
    pass
else:
    raise AssertionError("annotations GitHub didn't give were guessed at")
ANNOTATIONS[41] = ["The runner has received a shutdown signal. This can happen when the runner service is stopped."]
assert state(timeout) == stopped(CHECK)
ANNOTATIONS[41] = ["The run was canceled by @sam."]
assert state(timeout) == stopped(CHECK)
ANNOTATIONS.clear()

# Nothing stands in for a newer run that ran through. Alone, as when cancelled
# by hand or its runner shut down under it; two cancelled runs, which mustn't
# clear each other; another workflow's green; a check run of no workflow,
# whose suite says nothing. Pending, not red: it waits to be run again, and
# no fix job is sent for a run that found nothing.
assert state(old) == stopped(CHECK)
status, landed = at(clean(checks=old))
assert not landed and JOBS == [] and "waiting for CI: check / check (cancelled, to be run again)" in status, (status, JOBS)
assert state(old, whole(suite="s2", conclusion=CANCEL)) == stopped(CHECK)
assert state(old, [check("report", suite="s2", workflow="report")]) == stopped(CHECK)
other_app = check("scan", CANCEL, app="some-app", suite="a1")
other_app["checkSuite"]["workflowRun"] = None
green_app = check("scan", app="some-app", suite="a2")
green_app["checkSuite"]["workflowRun"] = None
assert state([other_app], [green_app]) == stopped("scan")
# A suite that completed without doing the check's work: a caller skipped on a
# title edit, the gate skipped with no docs lane in its place, only other jobs
# run, or a docs lane that was itself skipped. Whether it came before or after.
for why, run in [
    ("caller skipped", [check("check", "SKIPPED", suite="s2")]),
    ("gate skipped", [check("check / lane into main", suite="s2"), check(CHECK, "SKIPPED", suite="s2")]),
    ("other jobs", [check("check / commits", suite="s2")]),
    ("docs lane skipped", [check(CHECK, "SKIPPED", suite="s2"),
                           check("check / docs only into main from " + "f" * 40, "SKIPPED", suite="s2")]),
]:
    assert state(old, run) == stopped(CHECK), why
    assert state(run, old) == stopped(CHECK), why
    status, landed = at(clean(checks=old + run))
    assert not landed and JOBS == [] and "waiting for CI" in status, (why, status, JOBS)
# The docs lane standing in for the gate it skipped does count, and its red is
# the head's.
status, landed = at(clean(checks=old + docs(suite="s2"), files=("README.md",)))
assert landed and "CI is green (docs only)" in status, status
status, landed = at(clean(checks=old + docs(suite="s2", conclusion="FAILURE"), files=("README.md",)))
assert not landed and "CI is red" in status, status
# From another fork, that docs lane judged another diff: it clears nothing.
assert state(old, docs(fork="e" * 40, suite="s2"))[0] == "pending"
# Outside the shared CI, no lane to ask for: by name, and only a job that ran.
build = check("build", CANCEL, suite="b1", workflow="b")
assert state([build], [check("build", suite="b2", workflow="b")]) == ("green", [])
assert state([build], [check("build", "SKIPPED", suite="b2", workflow="b"), check("lint", suite="b2", workflow="b")]) == stopped("build")
# A push's or a dispatch's run has no lane, so it judged no PR, however its jobs
# are named and went, before the cancelled gate or after. A lane cancelled
# before its name was filled in doesn't let one through either.
UNFILLED = "check / lane into ${{ github.event.pull_request.base.ref }}"
push = [check(UNFILLED, "SKIPPED", suite="s2"), check(CHECK, suite="s2")]
assert state(old, push) == stopped(CHECK)
assert state(push, old) == stopped(CHECK)
assert state(old, [check(CHECK, suite="s2")]) == stopped(CHECK)
nameless = [check(UNFILLED, CANCEL, suite="s1"), check(CHECK, CANCEL, suite="s1")]
assert state(nameless, push) == stopped(UNFILLED, CHECK)
assert state(nameless, whole(suite="s2")) == ("green", [])
# A PR's lane that was itself skipped says nothing of the PR.
assert state(old, [check("check / lane into main", "SKIPPED", suite="s2"), check(CHECK, suite="s2")]) == stopped(CHECK)
# A real run stands beside one of those, and its red is still the head's.
assert state(old, push, whole(suite="s3")) == ("green", [])
assert state(old, push, whole(suite="s3", conclusion="FAILURE")) == ("red", [CHECK])

# The docs lane replaces its own call's whole gate and nothing else: not a
# job it skipped beside itself, not the commits check, not a sibling call's
# gate or a matrix leg's.
scan = check("check / scan", CANCEL, suite="s1")
assert state(old + [scan], docs(suite="s2") + [check("check / scan", "SKIPPED", suite="s2")]) == stopped("check / scan")
commits = check("check / commits", CANCEL, suite="s1")
assert state(old + [commits], docs(suite="s2") + [check("check / commits", "SKIPPED", suite="s2")]) == stopped("check / commits")
assert state(old + [commits], docs(suite="s2") + [check("check / commits", suite="s2")]) == ("green", [])
sibling = whole(suite="s1") + [check("other / lane into main", suite="s1"), check("other / check", CANCEL, suite="s1")]
later = docs(suite="s2") + [check("other / lane into main", suite="s2"), check("other / check", "SKIPPED", suite="s2")]
assert state(sibling, later) == stopped("other / check")
leg = [check("check (a) / lane into main", suite="s1"), check("check (a) / check", CANCEL, suite="s1")]
assert state(leg, docs(suite="s2")) == stopped("check (a) / check")
# Nor does one call's lane vouch for a sibling's job of the same name's run.
assert state([check("other / check", CANCEL, suite="s1"), check("other / lane into main", suite="s1")],
             [check("check / lane into main", suite="s2"), check("other / check", suite="s2")]) == stopped("other / check")
# The whole gate replaces a cancelled docs lane in turn, a docs lane another
# whose name was never filled in, and the replacement's red is the head's.
assert state(docs(suite="s1", conclusion=CANCEL), whole(suite="s2")) == ("green", [])
assert state(docs(suite="s1", conclusion=CANCEL), whole(suite="s2", conclusion="FAILURE")) == ("red", [CHECK])
blank = "check / docs only into ${{ github.event.pull_request.base.ref }} from ${{ needs.lane.outputs.fork }}"
assert state(whole(suite="s1")[:1] + [check(blank, CANCEL, suite="s1")], docs(suite="s2")) == ("green", [])

# A job beside the shared CI's call, top-level or another reusable workflow's,
# has no lane to ask for: its own run clears it in either order, whichever
# suite the shared CI's call passed in, a skip doesn't and its red stays red.
# The shared CI's own checks keep their lane, scoped to their call.
for name in ("browser-tests", "e2e / browser-tests"):
    gone = [check(name, CANCEL, suite="s1")]
    assert state(whole(suite="s1") + gone, whole(suite="s2") + [check(name, suite="s2")]) == ("green", []), name
    assert state(whole(suite="s2") + [check(name, suite="s2")], whole(suite="s1") + gone) == ("green", []), name
    assert state(whole(suite="s1") + gone, whole(suite="s2") + [check(name, "SKIPPED", suite="s2")]) == stopped(name), name
    assert state(whole(suite="s1") + gone, whole(suite="s2") + [check(name, "FAILURE", suite="s2")]) == ("red", [name]), name
    assert state(whole(suite="s1") + gone, whole(suite="s2")) == stopped(name), name
assert state(old + [check("browser-tests", suite="s1")], push + [check("browser-tests", suite="s2")]) == stopped(CHECK)

# A run that judged another base isn't the PR's CI, so it clears nothing: the
# gate for the PR's own base never ran through.
assert state(old, whole("next", "s2")) == stopped(CHECK)
# One for the PR's base stands, whatever a cancelled run for another base says.
assert state(whole("next", "s0", conclusion=CANCEL), whole("main", "s1"), base="main") == ("green", [])

# A cancelled run nothing ran through since is run again, once, whatever its
# annotations say (no timeout among them): its workflow run, with one request.
# The allowance is GitHub's run_attempt, so nothing is kept here: a pass that
# sees the attempt going waits, and a later attempt cancelled is red, and a
# fix job's.
def hung(job, run=old, name=CHECK):
    return [{**c, "databaseId": job} if c["name"] == name else c for c in run]


def start():
    RERUNS.clear()
    RERUN_FAILS.clear()
    RUN_INFO.clear()
    fresh_pr_state()


def ended(key, attempt, conclusion="cancelled"):
    RUN_INFO[key] = {"run_attempt": attempt, "status": "completed", "conclusion": conclusion}


start()
status, _ = one_pass(clean(checks=hung(51)), 700)
assert RERUNS == ["s1"] and "waiting for CI" in status and not JOBS, (RERUNS, status, JOBS)
# The retry queued or running, the same pass seen again, or beside the original.
status, _ = one_pass(clean(checks=hung(51)), 760)
assert RERUNS == ["s1"] and "cancelled again" not in status and not JOBS, (RERUNS, status, JOBS)
status, _ = one_pass(clean(checks=hung(51) + going("s2")), 770)
assert RERUNS == ["s1"] and "cancelled again" not in status and not JOBS, (RERUNS, status, JOBS)
ended("s1", 2)
status, _ = one_pass(clean(checks=hung(52)), 800)
assert RERUNS == ["s1"] and "CI is red: check / check (cancelled again after a rerun)" in status, (RERUNS, status)
# The rerun passing ends it.
start()
one_pass(clean(checks=hung(51)), 700)
ended("s1", 2, "success")
status, landed = one_pass(clean(checks=whole(suite="s2")), 800)
assert RERUNS == ["s1"] and "CI is green" in status, (RERUNS, status)

# The allowance is the run's, not a PR's: a second PR on the head, a restart
# (state forgotten), and a request whose response was lost (it took, GitHub
# says attempt 2) all find the run already run again, and never ask a second.
start()
one_pass(clean(checks=hung(51)), 700)
fresh_pr_state()  # another PR's state, or this one after a restart
status, _ = one_pass(clean(checks=hung(51)), 760)
assert RERUNS == ["s1"] and "cancelled again" not in status, (RERUNS, status)
ended("s1", 2)
fresh_pr_state()
status, _ = one_pass(clean(checks=hung(52)), 800)
assert RERUNS == ["s1"] and "cancelled again after a rerun" in status, (RERUNS, status)
start()
ended("s1", 2)  # accepted before the steward's own request was lost or its pass died
status, _ = one_pass(clean(checks=hung(51)), 700)
assert RERUNS == [] and "cancelled again after a rerun" in status, (RERUNS, status)
start()
RERUN_FAILS["s1"] = RuntimeError("gh run rerun: HTTP 500: lost the answer")
one_pass(clean(checks=hung(51)), 700)
RERUN_FAILS.clear()
RUN_INFO["s1"] = {"run_attempt": 2, "status": "in_progress", "conclusion": None}  # it did take
status, _ = one_pass(clean(checks=hung(51)), 760)
assert RERUNS == [] and "cancelled again" not in status, (RERUNS, status)

# Another head's run is a run of its own, with its own allowance.
start()
one_pass(clean("a" * 40, checks=hung(51)), 700)
RUN_INFO.clear()
one_pass(clean("b" * 40, checks=hung(53, whole(suite="s9", conclusion=CANCEL))), 760)
assert RERUNS == ["s1", "s9"], RERUNS

# Several cancelled jobs of one run, jobs sharing a display name among them
# (twin jobs, matrix legs), are one request: GitHub starts one attempt for the
# run, and a second request for its jobs would be refused. A later attempt
# cancelling any of them is red.
def jobs(ids, run="s1"):
    out = []
    for i in ids:
        out += [{**c, "databaseId": i} for c in whole(suite=run, conclusion=CANCEL)[1:2]]
    return out


start()
status, _ = one_pass(clean(checks=jobs([51, 52])), 700)
assert RERUNS == ["s1"] and "waiting for CI" in status and not JOBS, (RERUNS, status, JOBS)
ended("s1", 2)
status, _ = one_pass(clean(checks=jobs([51]) + jobs([53])), 800)
assert RERUNS == ["s1"] and "cancelled again after a rerun" in status, (RERUNS, status)

# Runs of different workflows that share a job name keep a rerun each, and one
# cancelled again is red without making the other red.
def in_flow(flow, job):
    run = hung(job, whole(suite=f"s-{flow}", conclusion=CANCEL))
    for c in run:
        c["checkSuite"]["workflowRun"]["workflow"] = {"id": flow, "name": flow}
    return run


start()
a, b = in_flow("a", 61), in_flow("b", 62)
one_pass(clean(checks=a + b), 700)
assert sorted(RERUNS) == ["s-a", "s-b"], RERUNS
ended("s-b", 2)
ended("s-a", 2, "success")
status, _ = one_pass(clean(checks=a + in_flow("b", 63)), 760)
assert sorted(RERUNS) == ["s-a", "s-b"] and "CI is red: check / check (cancelled again after a rerun)" in status, (RERUNS, status)

# A run with a failure of its own isn't run again (that would run the failure
# again too): red as it is, whether the failure is a job's conclusion or a
# timeout. A run that isn't over is waited for, then run again.
start()
failed = hung(51, whole(suite="s1", conclusion=CANCEL))
failed[0]["conclusion"] = "FAILURE"
status, _ = one_pass(clean(checks=failed), 700)
assert RERUNS == [] and "CI is red" in status, (RERUNS, status)
start()
status, _ = one_pass(clean(checks=hung(51, old) + going("s1")), 700)
assert RERUNS == [] and "waiting for CI" in status, (RERUNS, status)
status, _ = one_pass(clean(checks=hung(51, old)), 760)
assert RERUNS == ["s1"], RERUNS

# A run GitHub says is still going isn't run again, whatever the rollup shows.
start()
RUN_INFO["s1"] = {"run_attempt": 1, "status": "in_progress", "conclusion": None}
status, _ = one_pass(clean(checks=hung(51)), 700)
assert RERUNS == [] and "waiting for CI" in status, (RERUNS, status)

# A rerun that fails keeps the ones before it: run A is asked once though
# run B raises after it, and a pass that fails later asks only B again.
start()
two = hung(71, whole(suite="a", conclusion=CANCEL)) + hung(72, whole(suite="b", conclusion=CANCEL))
RERUN_FAILS["b"] = C.Unavailable("github down")
try:
    one_pass(clean(checks=two), 700)
    raise AssertionError("an unavailable GitHub should end the pass")
except C.Unavailable:
    pass
RERUN_FAILS.clear()
one_pass(clean(checks=two), 760)
assert RERUNS == ["a", "b"], RERUNS
# A refusal for want of Actions write isn't a rerun: it says so, and is tried
# again once repaired. Any other (a run GitHub still thinks is going) is tried
# again too and says nothing of permission.
start()
RERUN_FAILS["s1"] = RuntimeError("gh run rerun: HTTP 403: Resource not accessible by integration")
status, _ = one_pass(clean(checks=hung(51)), 700)
assert RERUNS == [] and "Actions write" in status and not JOBS, (RERUNS, status, JOBS)
RERUN_FAILS["s1"] = RuntimeError("gh run rerun: HTTP 403: Resource not accessible by integration")
status, _ = one_pass(clean(checks=hung(51)), 730)
assert RERUNS == [] and "Actions write" in status, (RERUNS, status)
RERUN_FAILS.clear()
status, _ = one_pass(clean(checks=hung(51)), 760)
assert RERUNS == ["s1"] and "Actions write" not in status, (RERUNS, status)
start()
RERUN_FAILS["s1"] = RuntimeError("gh run rerun: HTTP 409: workflow run is still running")
status, _ = one_pass(clean(checks=hung(51)), 700)
assert RERUNS == [] and "Actions write" not in status and "waiting for CI" in status, (RERUNS, status)
RERUN_FAILS.clear()
one_pass(clean(checks=hung(51)), 760)
assert RERUNS == ["s1"], RERUNS

# A check that isn't an Actions job (another App's, no workflow) isn't sent to
# gh run rerun: it waits as before.
start()
other = [{**check("some-app", CANCEL), "databaseId": 81, "checkSuite": {"id": "x", "status": "COMPLETED", "app": {"slug": "some-app"}}}]
status, _ = one_pass(clean(checks=other), 700)
assert RERUNS == [] and "waiting for CI" in status and not JOBS, (RERUNS, status, JOBS)

print("cancelled runs: ok")
