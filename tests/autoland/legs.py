"""burrow's shared CI with its gate split across legs (flake-check.yml's
`legs`): the job named check, which gives the gate's verdict, needs every
leg, so it has no check run until the last leg ends. For that moment every
check run on the head has passed while its run hasn't: pending, not green,
so nothing lands before the verdict. And a leg is the gate's work, as the
job named check is: a newer run's gate clears a cancelled leg, whatever its
lane and however many legs it has, and nothing else does."""

from docs import at, check, docs, whole
from harness import C, clean
from prs import ci_state

CANCEL = "CANCELLED"


def legs(done="COMPLETED", verdict="SUCCESS", leg="SUCCESS", suite="s1", n=4, base="main"):
    """One run of the shared CI on n legs. verdict None: the check job
    hasn't started. leg: what the third leg said, CANCELLED for every leg,
    as a newer run on the head cancels them all (the check job is then
    skipped)."""
    if leg == CANCEL:
        verdict = "SKIPPED"
    runs = [check(f"check / lane into {base}", suite=suite, done=done), check("check / commits", suite=suite, done=done)]
    runs += [check(f"check / leg {k} of {n}", leg if k == 3 or leg == CANCEL else "SUCCESS", suite=suite, done=done)
             for k in range(1, n + 1)]
    return runs + ([check("check / check", verdict, suite=suite, done=done)] if verdict else [])


def state(*runs):
    with C.acting_for("sam/r"):
        return ci_state(clean(checks=[c for run in runs for c in run]), repo="sam/r")[:2]


# Every leg passed and the check job hasn't started: CI is still going, and
# nothing lands.
assert state(legs("IN_PROGRESS", None)) == ("pending", ["flake (still running)"])
status, landed = at(clean(checks=legs("IN_PROGRESS", None)))
assert not landed and "waiting for CI: flake (still running)" in status, status
# The verdict in: it lands, or the head's red is the check job's.
assert state(legs()) == ("green", [])
status, landed = at(clean(checks=legs()))
assert landed and "CI is green" in status, status
assert state(legs(verdict="FAILURE")) == ("red", ["check / check"])
# A leg that failed is red at once, before the verdict.
assert state(legs("IN_PROGRESS", None, leg="FAILURE")) == ("red", ["check / leg 3 of 4"])
# A check run of no workflow has no run to wait for.
app = check("scan", app="some-app", suite="a1", done="IN_PROGRESS")
app["checkSuite"]["workflowRun"] = None
assert state([app]) == ("green", [])

# Cancelled legs: a newer run's gate does their work, on as many legs or
# another number of them, on one leg (its job named check), or in the docs
# lane once the base has taken the PR's code; its red is the head's.
old = legs(leg=CANCEL)
for why, new in [("same legs", legs(suite="s2")), ("three legs", legs(suite="s2", n=3)),
                 ("one leg", whole(suite="s2")), ("docs lane", docs(suite="s2"))]:
    assert state(old, new) == ("green", []), why
    assert state(new, old) == ("green", []), why
assert state(old, legs(suite="s2", n=3, verdict="FAILURE")) == ("red", ["check / check"])
assert state(old, whole(suite="s2", conclusion="FAILURE")) == ("red", ["check / check"])
# Nothing else does: the run's other jobs, a sibling call's legs, a run for
# another base, legs that were skipped, or a newer run cancelled in turn.
LEG = "check / leg 1 of 4"
sibling = [check("other / lane into main", suite="s2")] + [check(f"other / leg {k} of 4", suite="s2") for k in range(1, 5)]
for why, new in [("other jobs", [check("check / lane into main", suite="s2"), check("check / commits", suite="s2")]),
                 ("sibling call", sibling), ("another base", legs(suite="s2", base="next")),
                 ("skipped", [check("check / lane into main", suite="s2")]
                  + [check(f"check / leg {k} of 4", "SKIPPED", suite="s2") for k in range(1, 5)]),
                 ("cancelled too", legs(suite="s2", leg=CANCEL))]:
    got = state(old, new)
    assert got[0] == "red" and LEG in got[1], (why, got)
print("legs: ok")
