"""What's open on a PR an author put first goes to a Claude job in the step
that finds it (step.handle's first, step_actions' hand_off), rather than
after handoffAfter: findings its reviewers just sent back, open threads.
Never a second job on its branch while one's at work, never while it's held,
and not while the Claude session that opened it has it."""

from harness import C, JOBS, POSTED, STATES, T, fresh_pr_state, mkpr, run, stand_in, worker

stand_in()

finding = {"result": "review", "answer": {"findings": [{"path": "a.py"}]}}

# An ordinary PR's open thread waits handoffAfter for its author; a first
# PR's goes to a job then and there, and a second look at it while that job
# is at work (the same event twice, say) starts none.
fresh_pr_state()
status, _ = run(mkpr(threads=[False]), 600)
assert JOBS == [] and "a Claude job takes it at" in status, status
fresh_pr_state()
status, _ = run(mkpr(threads=[False]), 600, first=True)
assert len(JOBS) == 1 and "a Claude job has it since" in status, (JOBS, status)
status, _ = run(mkpr(threads=[False]), 610, first=True)
assert JOBS == [] and "a Claude job has it since" in status, (JOBS, status)

# Findings its reviewers send back: the step that posts them hands them to
# one job, in that same step.
for first in (False, True):
    fresh_pr_state()
    run(mkpr(), 0, first=first)
    run(mkpr(), 600, first=first)
    worker(rival=finding)
    status, _ = run(mkpr(), 700, first=first)
    assert ("rival", mkpr()["headRefOid"], 1) in POSTED, POSTED
    assert len(JOBS) == first, (first, JOBS, status)
    if first:
        assert "a Claude job has it since" in status, status
        # Its job still at work, the next step (the review's own event, a
        # duplicate) starts no other.
        status, _ = run(mkpr(threads=[False]), 710, first=True)
        assert JOBS == [] and "has it since" in status, (JOBS, status)
        # Once it's over without a push, it's handed once more, at once.
        STATES[f"job-1"] = "done"
        run(mkpr(threads=[False]), 720, first=True)
        status, _ = run(mkpr(threads=[False]), 730, first=True)
        assert len(JOBS) == 1, (JOBS, status)
        STATES.clear()

# Held, nothing's handed, first or not.
fresh_pr_state()
status, _ = run(mkpr(threads=[False], labels=["hold"]), 600, first=True)
assert JOBS == [] and "hold" in status.lower(), status

# A PR whose own Claude session opened it is that session's for its wait:
# it's at work on the branch, as a job would be.
fresh_pr_state()
linked = "Does it.\n\nhttps://claude.ai/code/session_01AbCdEf"
status, _ = run(mkpr(threads=[False], body=linked), 600, first=True)
assert JOBS == [] and "the Claude session that opened i" in status, status
assert C.WAKE["sam/r"] > T + 600, C.WAKE

print("a first PR's findings go to one job at once: passed")
