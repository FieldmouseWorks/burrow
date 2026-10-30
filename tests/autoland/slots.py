"""proofSlots: a repo's proofs that take the whole machine run a few at a
time, the PRs labelled first ahead of the rest and the oldest first within
each, the rest waiting their turn rather than their timeout; a job's end
kicks the pass that starts the next. A slot is held by a job until it ends,
whatever its PR does meanwhile, and a turn in the line only by a PR waiting
in this same pass."""

import itertools
import json

import proofs as P
from harness import C, CI_GREEN, JOBS, S, STATES, clean, fresh_pr_state, info, mkpr, pr_state, run, tmp

kept = C.job
ids = itertools.count(1)
C.job = lambda repo, task, clone=False: JOBS.append(task) or f"job-{next(ids)}"  # one id a job, across passes
S.CFG["repos"]["sam/r"] = {"proofJobs": {"remi-proof": "Run `just proof` and post the remi-proof status."}, "proofSlots": 1}
proof = CI_GREEN + [{"__typename": "StatusContext", "context": "remi-proof", "state": "PENDING", "description": ""}]
other = "c" * 40  # another head
RUNNING = "a Claude job is running remi-proof"


def line(n, t=700, **kw):
    return run(clean(checks=proof, number=n, **kw), t)[0]


def ahead(k):
    return f"waiting its turn to run remi-proof ({k} ahead)"


def ends(n):
    """PR n's latest proof job ends."""
    STATES[pr_state(n)["provedJob"]] = "done"


# Oldest first: one job, the others in line behind it.
fresh_pr_state()
P.begin("sam/r", {1, 2, 3})
assert RUNNING in line(1) and len(JOBS) == 1, JOBS
assert ahead(1) in line(2) and JOBS == []
assert ahead(2) in line(3) and JOBS == []
# One closed meanwhile holds nothing up, its job or not.
P.begin("sam/r", {2, 3})
assert RUNNING in line(2) and len(JOBS) == 1, JOBS
assert ahead(1) in line(3) and JOBS == []
# A new head on a PR whose job still runs: that job keeps its slot, from
# the PR itself and from those after it.
P.begin("sam/r", {2, 3})
assert ahead(1) in line(2, 800, sha=other) and JOBS == [], JOBS
assert ahead(2) in line(3, 800) and JOBS == []
# Marked a draft meanwhile, it still has it.
P.begin("sam/r", {2, 3})
assert "Draft" in run(clean(checks=proof, number=2, sha=other, draft=True), 900)[0]
assert ahead(1) in line(3, 900) and JOBS == []
# Once it ends, the next in line starts.
ends(2)
P.begin("sam/r", {2, 3})
assert RUNNING in line(2, 1000, sha=other) and len(JOBS) == 1, JOBS
assert ahead(1) in line(3, 1000) and JOBS == []
ends(2)
P.begin("sam/r", {2, 3})
line(2, 1100, sha=other)  # the first look at a job that ended without its verdict
assert RUNNING in line(3, 1100) and len(JOBS) == 1, JOBS
# Its job ended without the verdict: started again, but only in its turn.
P.begin("sam/r", {2, 3})
assert ahead(1) in line(2, 1200, sha=other) and JOBS == [], JOBS
ends(3)
P.begin("sam/r", {2, 3})
assert "running remi-proof again" in line(2, 1300, sha=other) and len(JOBS) == 1, JOBS

# Every job a PR started holds a slot, however many heads and launches
# since: two slots, two jobs on one PR, and the next PR waits.
fresh_pr_state()
S.CFG["repos"]["sam/r"]["proofSlots"] = 2
P.begin("sam/r", {1, 2})
assert RUNNING in line(1) and len(JOBS) == 1
P.begin("sam/r", {1, 2})
assert RUNNING in line(1, 800, sha=other) and len(JOBS) == 1
assert len(pr_state(1)["proofsLive"]) == 2, pr_state(1)
assert ahead(2) in line(2, 800) and JOBS == []
S.CFG["repos"]["sam/r"]["proofSlots"] = 1

# One from before proofsLive was kept: its latest job still holds a slot.
fresh_pr_state()
P.begin("sam/r", {1, 2})
line(1)
st = pr_state(1)
del st["proofsLive"]
json.dump(st, open(f"{tmp}/state/pr-sam_r-1.json", "w"))
assert ahead(1) in line(2) and JOBS == []

# A PR that waited its turn and then stopped being one to prove (held,
# drafted, the owner's, not needed, findings or red or ordinary pending CI,
# or its proof done) holds no turn: the next starts once the job ends.
hold, owner = S.CFG["labels"]["hold"], S.CFG["labels"]["owner"]
red = [{"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "FAILURE"}]
done = CI_GREEN + [{"__typename": "StatusContext", "context": "remi-proof", "state": "SUCCESS", "description": ""}]
busy = proof + [{"__typename": "CheckRun", "name": "build", "status": "IN_PROGRESS", "conclusion": None}]
for kw, obsolete in (({"draft": True}, False), ({"labels": (hold,)}, False), ({"labels": (owner,)}, False),
                     ({}, True), ({"threads": [False]}, False), ({"checks": red}, False),
                     ({"checks": busy}, False), ({"checks": done}, False)):
    fresh_pr_state()
    P.begin("sam/r", {1, 2, 3})
    line(1)
    assert ahead(1) in line(2) and ahead(2) in line(3)
    ends(1)
    P.begin("sam/r", {2, 3})  # the first one landed
    status, _ = run(clean(**{"checks": proof, **kw}, number=2), 800, obsolete)
    assert "waiting its turn" not in status, (kw, status)
    status = line(3, 800)
    assert RUNNING in status and len(JOBS) == 1, (kw, status, JOBS)

# Labelled first: ahead of every PR without the label, whatever their
# numbers, and the oldest first among the labelled.
first = S.CFG["labels"]["first"]
fresh_pr_state()
P.begin("sam/r", {1, 2, 3, 4}, {3, 4})
assert RUNNING in line(3, labels=(first,)) and len(JOBS) == 1, JOBS
assert ahead(1) in line(4, labels=(first,)) and JOBS == []
assert ahead(2) in line(1) and JOBS == []
assert ahead(3) in line(2) and JOBS == []
# Once its job ends, the next labelled one starts, ahead of the older ones.
ends(3)
P.begin("sam/r", {1, 2, 4}, {4})
assert RUNNING in line(4, 800, labels=(first,)) and len(JOBS) == 1, JOBS
assert ahead(1) in line(1, 800) and ahead(2) in line(2, 800) and JOBS == []
# The pass takes a repo's PRs in that order, so a free slot goes to the
# first in line.
order = []
kept_fns = (S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, S.R.unhook_gone, S.R.hook_projects, S.step.handle)
S.R.discover = lambda: [{"repo": "sam/r"}]
S.R.settle_repo = lambda r, known: False
S.R.publish = S.R.unhook_gone = lambda *a: None
S.R.hook_projects = lambda repos: []
S.open_prs = lambda owner, name: (info, [mkpr(number=k, labels=lb) for k, lb in ((2, ()), (9, (first,)), (1, ()), (7, (first,)))])
S.step.handle = lambda repo, pr, *a: order.append(pr["number"])
S.main()
S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, S.R.unhook_gone, S.R.hook_projects, S.step.handle = kept_fns
assert order == [7, 9, 1, 2], order

# Unset, no limit.
fresh_pr_state()
del S.CFG["repos"]["sam/r"]["proofSlots"]
P.begin("sam/r", {1, 2})
assert RUNNING in line(1) and RUNNING in line(2), JOBS
del S.CFG["repos"]["sam/r"]
C.job = kept
P.OPEN.clear()
P.FIRST.clear()
P.WAITING.clear()
STATES.clear()
fresh_pr_state()
print("proof slots: ok")
