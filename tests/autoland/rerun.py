"""A check run again on the same head (house, when the PR's body is edited;
any run someone runs again): each run's suite stays in the rollup, so the
newer run of the same check by the same workflow is the word on the head,
and the older one's failure is no longer red. The newer one's own red, or
its still running, is the head's. An older success doesn't clear a newer
failure, a namesake from another workflow clears nothing, and a check run
with no id has no newer run to go by."""

from docs import at, check
from harness import JOBS, C, clean
from prs import ci_state

HOUSE = "house"


def run(conclusion, id, suite, workflow="house", status="COMPLETED"):
    return {**check(HOUSE, conclusion, status=status, suite=suite, workflow=workflow,
                    done=status), "databaseId": id}


def state(*checks):
    with C.acting_for("sam/r"):
        return ci_state(clean(checks=list(checks)), repo="sam/r")[:2]


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
