"""A cancelled check run: a PR's new run on the same head (closed and reopened
while CI ran, say) cancels its last one, whose suite stays on the head. It goes
by what its workflow did after it: another suite that has done that check's
work for the PR's diff clears it, one still going leaves CI pending, and with
neither it stays not green, as when someone cancelled the run by hand."""

from docs import at, check, docs, whole
from harness import JOBS, clean
from prs import ci_state

CANCEL, CHECK = "CANCELLED", "check / check"


def state(*checks, base="main"):
    return ci_state(clean(checks=[c for run in checks for c in run], base=base))[:2]


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
# A run that judged another base isn't the PR's CI, so it clears nothing: the
# gate for the PR's own base never ran through.
assert state(old, whole("next", "s2")) == ("red", [CHECK])
# One for the PR's base stands, whatever a cancelled run for another base says.
assert state(whole("next", "s0", conclusion=CANCEL), whole("main", "s1"), base="main") == ("green", [])

print("cancelled runs: ok")
