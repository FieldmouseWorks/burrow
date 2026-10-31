"""Work on a head a PR no longer has is stopped: a verdict on that head
proves nothing about the one it has now. When its head moves on, its proof
jobs are stopped and give back their slot (unless one is building what the
new head's proof wants: builds.py), and so is its judge; a job stopped that
way never counts as one that broke or ended without its answer, on any head.
Its fix jobs carry on, since they push to the branch whatever its head. One
proj can't stop holds its slot and is tried again (strikes.py has the
retries)."""

import itertools

import proofs as P
from harness import C, CI_GREEN, JOBS, S, STATES, STOPPED, UNSTOPPABLE, clean, fresh_pr_state, mkpr, pr_state, run

kept = C.job
ids = itertools.count(1)
C.job = lambda repo, task, clone=False, **kw: JOBS.append(task) or f"sup-{next(ids)}"  # one id a job, across passes
S.CFG["repos"]["sam/r"] = {"proofJobs": {"remi-proof": "Run `just proof` and post the remi-proof status."}, "proofSlots": 1}
proof = CI_GREEN + [{"__typename": "StatusContext", "context": "remi-proof", "state": "PENDING", "description": ""}]
A, B, C2 = "a" * 40, "b" * 40, "c" * 40
RUNNING = "a Claude job is running remi-proof"
gated = {"files": ["secrets/app.yaml"], "number": 6}  # a one-way path: a Claude job judges it


def line(n, t, sha=A, **kw):
    return run(clean(checks=proof, number=n, sha=sha, **kw), t)[0]


def fresh():
    fresh_pr_state()
    STOPPED.clear()
    STATES.clear()


# PR 1's proof runs on A, and PR 2 waits behind it. PR 1 is pushed again:
# its job on A is stopped, and B's proof takes the slot at once, PR 2 still
# behind it.
fresh()
P.begin("sam/r", {1, 2})
assert RUNNING in line(1, 700) and len(JOBS) == 1, JOBS
on_a = pr_state(1)["provedJob"]
assert "(1 ahead)" in line(2, 700) and JOBS == [], JOBS
P.begin("sam/r", {1, 2})
status = line(1, 800, B)
assert STOPPED == [on_a] and RUNNING in status and len(JOBS) == 1, (STOPPED, status, JOBS)
assert "(1 ahead)" in line(2, 800) and JOBS == [], JOBS
# The stopped one didn't break, and isn't B's: no strike, no retry, and B's
# job is the one live job.
st = pr_state(1)
assert st["strikes"] == {} and st["proofsLive"] == [st["provedJob"]] != [on_a] and "again" not in status, (st, status)

# A head it comes back to is proved afresh: the job stopped there neither
# runs on nor broke. A is pushed over by C (its reviewers still to look),
# then A comes back.
fresh()
P.begin("sam/r", {1})
line(1, 700)
on_a = pr_state(1)["provedJob"]
run(mkpr(number=1, sha=C2, checks=proof), 800)
assert STOPPED == [on_a] and JOBS == [], (STOPPED, JOBS)
P.begin("sam/r", {1})
status = line(1, 900)
assert RUNNING in status and "again" not in status and len(JOBS) == 1, (status, JOBS)
status = line(1, 1000)
assert RUNNING in status and JOBS == [] and "lost" not in pr_state(1)["strikes"], (status, pr_state(1))

# One proj can't stop holds its slot, and is tried again: once it's
# stopped, the new head's proof starts.
fresh()
P.begin("sam/r", {1, 2})
line(1, 700)
on_a = pr_state(1)["provedJob"]
UNSTOPPABLE.add(on_a)
P.begin("sam/r", {1, 2})
status = line(1, 800, B)
assert STOPPED == [] and "(1 ahead)" in status and JOBS == [], (STOPPED, status, JOBS)
assert pr_state(1)["strikes"] == {"stop": 1}, pr_state(1)
UNSTOPPABLE.clear()
P.begin("sam/r", {1, 2})
status = line(1, 900, B)
assert STOPPED == [on_a] and RUNNING in status and len(JOBS) == 1, (STOPPED, status, JOBS)
assert "stop" not in pr_state(1)["strikes"], pr_state(1)

# Its judge on the last head is stopped too, and the new head is judged by
# a job of its own, its first. (The repo has no proofs to run.)
del S.CFG["repos"]["sam/r"]
fresh()
run(clean(**gated), 700)
judge = pr_state(6)["judgedJob"]
status, _ = run(clean(sha=B, **gated), 800)
assert STOPPED == [judge] and len(JOBS) == 1 and "judging it" in status and "again" not in status, (STOPPED, status, JOBS)
# One that has ended already is left alone.
fresh()
run(clean(**gated), 700)
STATES[pr_state(6)["judgedJob"]] = "done"
run(clean(sha=B, **gated), 800)
assert STOPPED == [], STOPPED

# A fix job carries on: it pushes to the branch, whatever its head.
fresh()
run(clean(threads=[False]), 0)
run(clean(threads=[False]), 7200)
assert len(JOBS) == 1 and pr_state()["handedJob"], (JOBS, pr_state())
run(clean(sha=B, threads=[False]), 7300)
assert STOPPED == [], STOPPED

C.job = kept
P.OPEN.clear()
P.WAITING.clear()
STATES.clear()
STOPPED.clear()
fresh_pr_state()
print("superseded heads: ok")
