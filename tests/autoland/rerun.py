"""A check run again on the same head (house, when the PR's body is edited;
any run someone runs again): each run's suite stays in the rollup, so the
newer run of the same check by the same workflow is the word on the head,
and the older one's failure is no longer red. The newer one's own red, or
its still running, is the head's. An older success doesn't clear a newer
failure, a namesake from another workflow clears nothing, and a check run
with no id has no newer run to go by."""

from docs import at, check, docs
from harness import ANNOTATIONS, JOBS, C, clean
from prs import ci_state

HOUSE = "house"


def run(conclusion, id, suite, workflow="house", status="COMPLETED"):
    return {**check(HOUSE, conclusion, status=status, suite=suite, workflow=workflow,
                    done=status), "databaseId": id}


def state(*checks):
    with C.acting_for("sam/r"):
        return ci_state(clean(checks=list(checks)), repo="sam/r", moved=())[:2]


failed, passed = run("FAILURE", 10, "s1"), run("SUCCESS", 11, "s2")
# The newer run passed: green, in whichever order the rollup lists them, and
# it lands with no fix job.
assert state(failed, passed) == ("green", []), state(failed, passed)
assert state(passed, failed) == ("green", [])
status, landed = at(clean(checks=[failed, passed]))
assert landed and JOBS == [] and "CI is red" not in status, (status, JOBS)
# The newer run's own failure is the head's, and an older pass clears nothing.
assert state(run("SUCCESS", 10, "s1"), run("FAILURE", 11, "s2")) == ("red", [HOUSE])
# Still running: pending, not red.
assert state(failed, run(None, 11, "s2", status="IN_PROGRESS"))[0] == "pending"
# Another workflow's check of the same name speaks for its own workflow only.
assert state(failed, run("SUCCESS", 11, "s2", workflow="other")) == ("red", [HOUSE])
# Without ids there's no telling which is newer: the failure stands.
bare = [{k: v for k, v in c.items() if k != "databaseId"} for c in (failed, passed)]
assert state(*bare) == ("red", [HOUSE])


# A skip performed no work. It cannot replace a failure, timeout,
# cancellation or running check, even with unrelated green CI present.
for conclusion, status, expected in [
    ("FAILURE", "COMPLETED", "red"),
    ("TIMED_OUT", "COMPLETED", "red"),
    ("CANCELLED", "COMPLETED", "pending"),
    (None, "IN_PROGRESS", "pending"),
]:
    old = run(conclusion, 10, "s1", status=status)
    skipped = run("SKIPPED", 11, "s2")
    for checks in ((old, skipped), (skipped, old)):
        for extra in ((), (check("lint", workflow="lint"),)):
            assert state(*checks, *extra)[0] == expected, (conclusion, state(*checks, *extra))

# Only later attempts may replace a check. A cancellation cannot be cleared
# by a predecessor that supersession discarded, in either rollup order.
for chain in [
    [run("FAILURE", 10, "s1"), run("CANCELLED", 11, "s2")],
    [run("FAILURE", 10, "s1"), run("SUCCESS", 11, "s2"), run("CANCELLED", 12, "s3")],
    [run("CANCELLED", 10, "s1"), run("CANCELLED", 11, "s2"), run("CANCELLED", 12, "s3")],
]:
    for checks in (chain, list(reversed(chain))):
        for extra in ((), (check("lint", workflow="lint"),)):
            assert state(*checks, *extra)[0] == "pending", state(*checks, *extra)
    latest = max(chain, key=lambda c: c["databaseId"])
    ANNOTATIONS[latest["databaseId"]] = ["has exceeded the maximum execution time"]
    assert state(*chain)[0] == "red", state(*chain)
    # A run still going doesn't hide a known timeout.
    assert state(*chain, run(None, 13, "s4", status="IN_PROGRESS"))[0] == "red"
    # Completed later work does: no obsolete annotation request is needed.
    ANNOTATIONS[latest["databaseId"]] = C.Unavailable("annotations unavailable")
    assert state(*chain, run("SUCCESS", 14, "s5")) == ("green", [])
    ANNOTATIONS.clear()

# Shared CI requires affirmative evidence from the replacement's own call.
def gate(conclusion, id, suite, caller="check", lane="main", lane_conclusion="SUCCESS"):
    return [{**check(f"{caller} / lane into {lane}", lane_conclusion, suite=suite), "databaseId": id - 1},
            {**check(f"{caller} / check", conclusion, suite=suite), "databaseId": id}]


old = gate("FAILURE", 10, "s1")
for lane, conclusion in [("${{ github.event.pull_request.base.ref }}", "SKIPPED"),
                         ("main", "SKIPPED"), ("next", "SUCCESS")]:
    later = gate("SUCCESS", 20, "s2", lane=lane, lane_conclusion=conclusion)
    assert state(*old, *later)[0] == "red", state(*old, *later)
assert state(*old, gate("SUCCESS", 20, "s2")[1])[0] == "red"  # no lane
sibling = gate("SUCCESS", 20, "s2", caller="other") + [gate("SUCCESS", 30, "s2")[1]]
assert state(*old, *sibling)[0] == "red"
assert state(*old, *gate("SUCCESS", 20, "s2")) == ("green", [])
# Standalone work beside the shared call retains its independent identity.
assert state(*gate("SUCCESS", 10, "s1"), run("FAILURE", 12, "s1"),
             *gate("SUCCESS", 20, "s2"), run("SUCCESS", 22, "s2")) == ("green", [])

# Display names are not job IDs. Independent jobs/matrix variants sharing
# one name all count. An explicit integrator job reference can identify a
# genuine retry within the same suite; an absent reference cannot.
for conclusion, expected in [("FAILURE", "red"), ("CANCELLED", "pending")]:
    a, b = run(conclusion, 10, "s1"), run("SUCCESS", 11, "s1")
    for checks in ((a, b), (b, a)):
        assert state(*checks)[0] == expected
    a["externalId"], b["externalId"] = "job-a", "job-b"
    assert state(a, b)[0] == expected
    b["externalId"] = "job-a"
    assert state(a, b) == ("green", [])
# Ambiguity in either run prevents a different suite from hiding a variant.
a, b = run("FAILURE", 10, "s1"), run("SUCCESS", 11, "s1")
assert state(a, b, run("SUCCESS", 20, "s2"))[0] == "red"
a, b = run("SUCCESS", 20, "s2"), run("SUCCESS", 21, "s2")
assert state(run("FAILURE", 10, "s1"), a, b)[0] == "red"


# A validated docs lane covers only its own call's gate work. Other jobs,
# other forks and changes to the workflow itself retain their evidence.
old = gate("FAILURE", 10, "s1")
later = docs(suite="s2")
assert state(*old, *later) == ("green", [])
for name in ("check / scan", "check / commits", "other / check", "check (a) / check"):
    extra = {**check(name, "FAILURE", suite="s1"), "databaseId": 11}
    assert state(*old, extra, *later)[0] == "red", name
assert state(*old, *docs(suite="s2", fork="e" * 40))[0] == "red"
with C.acting_for("sam/r"):
    # An unread rename set cannot certify a docs replacement either.
    assert ci_state(clean(checks=old + later), repo="sam/r")[0] == "red"
    assert ci_state(clean(checks=old + later, files=(".github/workflows/flake.yml",)),
                    repo="sam/r", moved=())[0] == "red"

# A standalone job in the same workflow is independent of a shared call's
# lane: a push may rerun that job without asserting it checked the PR gate.
old = gate("SUCCESS", 10, "s1") + [{**check("browser-tests", "FAILURE", suite="s1"), "databaseId": 12}]
later = gate("SUCCESS", 20, "s2", lane="${{ github.event.pull_request.base.ref }}", lane_conclusion="SKIPPED")
later += [{**check("browser-tests", suite="s2"), "databaseId": 22}]
assert state(*old, *later) == ("green", [])

# Missing suite identity leaves job membership ambiguous even with check IDs.
a, b = run("FAILURE", 10, "s1"), run("SUCCESS", 11, "s2")
a["checkSuite"]["id"] = None
assert state(a, b)[0] == "red"
