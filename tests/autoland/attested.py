"""A failed check run whose job's only failed step uploaded its attestation (a
Sigstore outage after the check itself passed) is run again once, as a
cancelled one is: pending meanwhile, and red if its attempt after the rerun
fails that way again. Any other failed step is the head's, red at once. The
job's own attempt says which attempt of its run it is, whatever the run's
conclusion (a job that continues on error leaves its run green), and a newer
run of its workflow on the head, still going, is waited for first."""

from docs import check, whole
from harness import ACTIONS_JOBS, JOBS, RERUNS, RUN_INFO, clean, fresh_pr_state, run as one_pass

CHECK = "check / check"
ATTEST = {"name": "Run actions/attest@1e69f48acb82d1966a394da916b4c1698aa569d6", "conclusion": "failure"}
GATE = {"name": "Run flake-gate", "conclusion": "failure"}
PASSED = {"name": "Run flake-gate", "conclusion": "success"}


def failed(job, steps, attempt=1):
    """The whole gate's run s1, its check failed in job `job` at `attempt`."""
    ACTIONS_JOBS[str(job)] = {"run_attempt": attempt, "steps": steps}
    return [{**c, "databaseId": job} if c["name"] == CHECK else c for c in whole(suite="s1", conclusion="FAILURE")]


def start():
    RERUNS.clear()
    RUN_INFO.clear()
    ACTIONS_JOBS.clear()
    fresh_pr_state()


def ended(attempt, conclusion="failure"):
    RUN_INFO["s1"] = {"run_attempt": attempt, "status": "completed", "conclusion": conclusion}


WAITING = f"waiting for CI: {CHECK} (attestation upload failed, run again)"
AGAIN = f"CI is red: {CHECK} (attestation upload failed again after a rerun)"

# Run again once, with its failed jobs only, and no fix job meanwhile.
start()
ended(1)
status, _ = one_pass(clean(checks=failed(61, [PASSED, ATTEST])), 700)
assert RERUNS == ["s1"] and WAITING in status and not JOBS, (RERUNS, status, JOBS)
status, _ = one_pass(clean(checks=failed(61, [PASSED, ATTEST])), 760)
assert RERUNS == ["s1"] and WAITING in status and not JOBS, (RERUNS, status, JOBS)
# Its attempt after the rerun failing the same way is red, with no second rerun.
ended(2)
status, _ = one_pass(clean(checks=failed(62, [PASSED, ATTEST], attempt=2)), 800)
assert RERUNS == ["s1"] and AGAIN in status, (RERUNS, status)
# The rerun passing ends it.
start()
ended(1)
one_pass(clean(checks=failed(61, [PASSED, ATTEST])), 700)
ended(2, "success")
status, _ = one_pass(clean(checks=whole(suite="s1")), 800)
assert RERUNS == ["s1"] and "CI is green" in status, (RERUNS, status)

# Its own step failing too, or alone, is red at once: nothing run again.
for steps in ([GATE], [GATE, ATTEST]):
    start()
    ended(1)
    status, _ = one_pass(clean(checks=failed(61, steps)), 700)
    assert RERUNS == [] and f"CI is red: {CHECK}" in status and "attestation" not in status, (steps, RERUNS, status)

# A job that continues on error fails inside a green run: still run again once,
# then red.
start()
ended(1, "success")
status, _ = one_pass(clean(checks=failed(61, [PASSED, ATTEST])), 700)
assert RERUNS == ["s1"] and WAITING in status, (RERUNS, status)
ended(2, "success")
status, _ = one_pass(clean(checks=failed(62, [PASSED, ATTEST], attempt=2)), 800)
assert RERUNS == ["s1"] and AGAIN in status, (RERUNS, status)
# A check run from an attempt the run has gone past waits for the newer one's.
start()
ended(2, "success")
status, _ = one_pass(clean(checks=failed(61, [PASSED, ATTEST])), 700)
assert RERUNS == [] and WAITING in status, (RERUNS, status)

# A newer run of its workflow on the head, started but not through (its lane
# done, its gate not yet begun): waited for, neither run again nor called red.
newer = [check("check / lane into main", suite="s2", done="IN_PROGRESS")]
for attempt in (1, 2):
    start()
    ended(attempt)
    status, _ = one_pass(clean(checks=failed(61, [PASSED, ATTEST], attempt) + newer), 700)
    assert RERUNS == [] and "waiting for CI" in status and "CI is red" not in status, (attempt, RERUNS, status)
