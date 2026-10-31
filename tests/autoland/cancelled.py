"""A cancelled check run: a PR's new run on the same head (closed and reopened
while CI ran, say) cancels its last one, whose suite stays on the head. It goes
by what its workflow did after it: another suite that has done that check's
work for the PR's diff clears it, one still going leaves CI pending, and with
neither it stays not green, as when someone cancelled the run by hand."""

from docs import at, check, docs, whole
from harness import JOBS, C, clean
from prs import ci_state

CANCEL, CHECK = "CANCELLED", "check / check"


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

# Nothing stands in for a newer run that ran through. Alone, as when cancelled
# by hand; two cancelled runs, which mustn't clear each other; another
# workflow's green; a check run of no workflow, whose suite says nothing.
assert state(old) == ("red", [CHECK])
status, landed = at(clean(checks=old))
assert not landed and "CI is red" in status, status
assert state(old, whole(suite="s2", conclusion=CANCEL)) == ("red", [CHECK])
assert state(old, [check("report", suite="s2", workflow="report")]) == ("red", [CHECK])
other_app = check("scan", CANCEL, app="some-app", suite="a1")
other_app["checkSuite"]["workflowRun"] = None
green_app = check("scan", app="some-app", suite="a2")
green_app["checkSuite"]["workflowRun"] = None
assert state([other_app], [green_app]) == ("red", ["scan"])
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
    assert state(old, run) == ("red", [CHECK]), why
    assert state(run, old) == ("red", [CHECK]), why
    status, landed = at(clean(checks=old + run))
    assert not landed and "CI is red" in status, (why, status)
# The docs lane standing in for the gate it skipped does count, and its red is
# the head's.
status, landed = at(clean(checks=old + docs(suite="s2"), files=("README.md",)))
assert landed and "CI is green (docs only)" in status, status
status, landed = at(clean(checks=old + docs(suite="s2", conclusion="FAILURE"), files=("README.md",)))
assert not landed and "CI is red" in status, status
# From another fork, that docs lane judged another diff: it clears nothing.
assert state(old, docs(fork="e" * 40, suite="s2"))[0] == "red"
# Outside the shared CI, no lane to ask for: by name, and only a job that ran.
build = check("build", CANCEL, suite="b1", workflow="b")
assert state([build], [check("build", suite="b2", workflow="b")]) == ("green", [])
assert state([build], [check("build", "SKIPPED", suite="b2", workflow="b"), check("lint", suite="b2", workflow="b")]) == ("red", ["build"])
# A push's or a dispatch's run has no lane, so it judged no PR, however its jobs
# are named and went, before the cancelled gate or after. A lane cancelled
# before its name was filled in doesn't let one through either.
UNFILLED = "check / lane into ${{ github.event.pull_request.base.ref }}"
push = [check(UNFILLED, "SKIPPED", suite="s2"), check(CHECK, suite="s2")]
assert state(old, push) == ("red", [CHECK])
assert state(push, old) == ("red", [CHECK])
assert state(old, [check(CHECK, suite="s2")]) == ("red", [CHECK])
nameless = [check(UNFILLED, CANCEL, suite="s1"), check(CHECK, CANCEL, suite="s1")]
assert state(nameless, push) == ("red", [UNFILLED, CHECK])
assert state(nameless, whole(suite="s2")) == ("green", [])
# A PR's lane that was itself skipped says nothing of the PR.
assert state(old, [check("check / lane into main", "SKIPPED", suite="s2"), check(CHECK, suite="s2")]) == ("red", [CHECK])
# A real run stands beside one of those, and its red is still the head's.
assert state(old, push, whole(suite="s3")) == ("green", [])
assert state(old, push, whole(suite="s3", conclusion="FAILURE")) == ("red", [CHECK])

# The docs lane replaces its own call's whole gate and nothing else: not a
# job it skipped beside itself, not the commits check, not a sibling call's
# gate or a matrix leg's.
scan = check("check / scan", CANCEL, suite="s1")
assert state(old + [scan], docs(suite="s2") + [check("check / scan", "SKIPPED", suite="s2")]) == ("red", ["check / scan"])
commits = check("check / commits", CANCEL, suite="s1")
assert state(old + [commits], docs(suite="s2") + [check("check / commits", "SKIPPED", suite="s2")]) == ("red", ["check / commits"])
assert state(old + [commits], docs(suite="s2") + [check("check / commits", suite="s2")]) == ("green", [])
sibling = whole(suite="s1") + [check("other / lane into main", suite="s1"), check("other / check", CANCEL, suite="s1")]
later = docs(suite="s2") + [check("other / lane into main", suite="s2"), check("other / check", "SKIPPED", suite="s2")]
assert state(sibling, later) == ("red", ["other / check"])
leg = [check("check (a) / lane into main", suite="s1"), check("check (a) / check", CANCEL, suite="s1")]
assert state(leg, docs(suite="s2")) == ("red", ["check (a) / check"])
# Nor does one call's lane vouch for a sibling's job of the same name's run.
assert state([check("other / check", CANCEL, suite="s1"), check("other / lane into main", suite="s1")],
             [check("check / lane into main", suite="s2"), check("other / check", suite="s2")]) == ("red", ["other / check"])
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
    assert state(whole(suite="s1") + gone, whole(suite="s2") + [check(name, "SKIPPED", suite="s2")]) == ("red", [name]), name
    assert state(whole(suite="s1") + gone, whole(suite="s2") + [check(name, "FAILURE", suite="s2")]) == ("red", [name]), name
    assert state(whole(suite="s1") + gone, whole(suite="s2")) == ("red", [name]), name
assert state(old + [check("browser-tests", suite="s1")], push + [check("browser-tests", suite="s2")]) == ("red", [CHECK])

# A run that judged another base isn't the PR's CI, so it clears nothing: the
# gate for the PR's own base never ran through.
assert state(old, whole("next", "s2")) == ("red", [CHECK])
# One for the PR's base stands, whatever a cancelled run for another base says.
assert state(whole("next", "s0", conclusion=CANCEL), whole("main", "s1"), base="main") == ("green", [])

print("cancelled runs: ok")
