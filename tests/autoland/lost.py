"""A Claude job that ends without the answer it was for. One that finished
its turn and gave none (a judge with no verdict, a fix that pushed nothing)
is started once more, and a second fails the PR's status for the owner. One
that stopped before it finished (it timed out, went quiet, crashed or Claude
errored) is the machine failing, not an answer: started again, later each
time, never failing the PR, and from the third in a row its wait is overdue
until a job finishes its turn, so the watchdog takes it."""

import itertools
import json
import os

import proofs as P
import waits
from harness import CI_GREEN, HELD, JOBS, STATES, STOPPED, UNSTOPPABLE, T, C, H, S, clean, fresh_pr_state, merged, mkpr, pr_state, run, stand_in, tmp

stand_in()
kept = C.job
ids = itertools.count(1)
ISSUED = []  # every job's id, in order: one id a job, across passes


def job(repo, task, clone=False, since=None, role=None):
    JOBS.append(task)
    ISSUED.append(f"job-{next(ids)}")
    return ISSUED[-1]


C.job = job
newer = "9" * 40
gated = {"files": ["secrets/app.yaml"], "number": 6}  # a one-way path: a Claude job judges it
LOST = H.STRIKES["lost"]


def ends(how):
    """The latest job ends, as proj says."""
    STATES[ISSUED[-1]] = how


def a_pass(pr, at):
    waits.begin()
    waits.read("sam/r", [pr])
    return run(pr, at)


def overdue(n=5):
    return waits.FAILING.get(f"sam/r#{n}")


def stall(n=5):
    """The PR's wait, as this pass publishes it for the watchdog."""
    waits.publish({"sam/r"}, {}, False)
    return json.load(open(f"{tmp}/state/watch.json"))["items"][f"sam/r#{n}"]


STUCK = []  # when the run of broken jobs' stall started, once it has


def one_stall(i):
    """From the third broken job in a row, the PR's wait is one stall, its
    start kept however the status reads (a retry booked, then running), so
    the watchdog's escalation of it carries on."""
    it = stall()
    if overdue() is None:
        return
    STUCK[:] = STUCK or [it["since"]]
    assert it["since"] == it["due"] == STUCK[0] and it["phase"].endswith("; " + LOST), (i, it, STUCK)


# A judge that ends without a verdict is started once more, after a second
# pass sees it ended; a second one that does the same fails the status and
# is listed for the owner.
fresh_pr_state()
run(clean(**gated), 700)
# Its end kicks a pass; one is booked for its deadline too (jobTimeout and
# five minutes), and every quarter hour past it while it hasn't ended.
assert C.WAKE["sam/r"] == T + 700 + 65 * 60, C.WAKE
run(clean(**gated), 800)
assert C.WAKE["sam/r"] == T + 700 + 65 * 60, C.WAKE
run(clean(**gated), 700 + 70 * 60)
assert C.WAKE["sam/r"] == T + 700 + 85 * 60, C.WAKE
ends("done")
status, _ = run(clean(**gated), 5000)
assert JOBS == [] and "judging it" in status, (status, JOBS)
status, digest = run(clean(**gated), 5100)
assert len(JOBS) == 1 and "judging it again (the last one ended without a verdict)" in status and digest == [], (status, digest)
status, _ = run(clean(**gated), 5200)
assert JOBS == [] and "judging it" in status and not merged(), status
ends("done")
status, digest = run(clean(**gated), 5300)
assert JOBS == [] and not merged() and digest == [], (status, JOBS, digest)
status, digest = run(clean(**gated), 5400)
assert JOBS == [] and [x[0] for x in digest] == ["Two Claude jobs ended without a verdict"], (status, digest)
assert "state=failure" in status and not merged(), status
# One still running, or one proj can't say about, isn't lost.
for state in ("running", None):
    fresh_pr_state()
    run(clean(**gated), 700)
    ends(state)
    for t in (800, 900):
        status, digest = run(clean(**gated), t)
    assert JOBS == [] and "judging it" in status and digest == [], (state, status)
STATES.clear()

# So do findings', red CI's and a dependency update's fix jobs that end
# without a push. A new head starts over, and so does one whose fix landed
# without a push (its threads answered).
ci_red = [{"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "FAILURE"}]
for mk, kw in ((clean, {"threads": [False]}), (clean, {"checks": ci_red}),
               (mkpr, {"threads": [False], "branch": "dependabot/npm/foo-1", "title": "Bump foo from 1.4 to 1.5",
                       "number": 11})):
    fresh_pr_state()
    run(mk(**kw), 0)
    run(mk(**kw), 7200)
    assert len(JOBS) == 1 and "isn't settled" in JOBS[0], (kw, JOBS)
    ends("done")
    run(mk(**kw), 7300)
    status, _ = run(mk(**kw), 7400)
    assert len(JOBS) == 1 and "state=pending" in status, (kw, status, JOBS)
    ends("done")
    run(mk(**kw), 7500)
    status, digest = run(mk(**kw), 7600)
    assert JOBS == [] and [x[0] for x in digest] == ["Two Claude jobs ended without fixing it"], (kw, digest)
    assert "state=failure" in status and "state=pending" not in status and not merged(), (kw, status)
    assert "Two Claude jobs ended without fixing it: " in status, status
    fixed = {k: v for k, v in kw.items() if k not in ("threads", "checks")}
    status, digest = run(mk(**fixed), 7700)
    assert "state=failure" not in status and digest == [], (kw, status, digest)
    status, digest = run(mk(**{**kw, "sha": newer}), 7800)
    assert "state=failure" not in status and digest == [], (kw, status, digest)
STATES.clear()


def awaits(*targets):
    """The latest job's turn ended awaiting targets, as `proj lifecycle` says."""
    HELD[ISSUED[-1]] = [f"await {t}" for t in targets]


# A fix job whose turn ended awaiting the fix it needs elsewhere (a PR in
# the repo this one pins, say) is still at work on it: proj resumes it once
# that changes. Not lost, not handed again however long that takes, and
# never two that failed (remi#881: three jobs on one head, each waiting on
# burrow's PR with the fix, and the PR on the stuck issue). It waits as on a
# hold, which nothing here moves. Awaiting only this PR is ending its turn.
threads = {"threads": [False]}
fresh_pr_state()
run(clean(**threads), 0)
run(clean(**threads), 7200)
assert len(JOBS) == 1, JOBS
ends("done")
awaits("sam/r#5", "sam/up#3")
for t in (7300, 7400, 7400 + 10 * 3600):
    status, digest = a_pass(clean(**threads), t)
    assert JOBS == [] and "its Claude job awaits sam/up#3" in status and "state=pending" in status, (t, status, JOBS)
    assert digest == [] and overdue() is None, (t, digest, waits.FAILING)
    it = stall()
    assert it["kick"] is None and it["due"] == it["since"] + 480 * 60 and it["claude"] is False, (t, it)
# It pushes a head of its own, then awaits again: the new head is its.
status, digest = a_pass(clean(sha=newer, **threads), 7400 + 11 * 3600)
assert JOBS == [] and "its Claude job on its findings awaits sam/up#3" in status and digest == [], (status, JOBS)
# Resumed, it's at work as any other is; ending with no await elsewhere, it
# counts as one that ended without fixing it, and the second is handed.
fresh_pr_state()
run(clean(**threads), 0)
run(clean(**threads), 7200)
ends("done")
awaits("sam/up#3")
status, _ = run(clean(**threads), 7300)
assert JOBS == [] and "awaits sam/up#3" in status, status
STATES[ISSUED[-1]] = "running"
HELD.pop(ISSUED[-1])
status, _ = run(clean(**threads), 7400)
assert JOBS == [] and "a Claude job has it since" in status, status
ends("done")
awaits("SAM/R#5")  # only this PR, however GitHub's names are cased


def claimed(**flags):
    """What claims (burrow-upkeep) says of the latest job between turns."""
    path = f"{tmp}/state/pr-sam_r-5.json"
    with open(path) as f:
        st = json.load(f)
    st["answering"][ISSUED[-1]].update(flags)
    with open(path, "w") as f:
        json.dump(st, f)


# Its turn's answers just went up, which resumes it: still at work on the
# branch, so nothing else is handed it (PSIV#459).
claimed(waits=True, resumes=True)
for t in (7500, 7600):
    status, digest = run(clean(**threads), t)
    assert JOBS == [] and "its Claude job awaits this PR's next change" in status and digest == [], (t, status, JOBS)
# Its turn left nothing new: it ended without fixing it, and the second is
# handed, once the first is stopped, so proj never resumes it on the same
# branch.
claimed(waits=True, resumes=False)
first = ISSUED[-1]
# One proj won't stop is no one retired: no second job is handed the branch
# while it's at large, on this pass or the next.
UNSTOPPABLE.add(first)
for t in (7700, 7800):
    status, digest = run(clean(**threads), t)
    assert JOBS == [] and first not in STOPPED, (t, status, JOBS)
UNSTOPPABLE.clear()
run(clean(**threads), 7900)
status, digest = run(clean(**threads), 8000)
assert len(JOBS) == 1 and "awaits" not in status and digest == [], (status, JOBS)
assert first in STOPPED, (first, STOPPED)
STATES.clear()
HELD.clear()


# A job whose turn ended with a question for a person (proj ask --default)
# is still at work, whatever its kind: their reply resumes it, and so does a
# reversible question's deadline, with its default. Taken for one that
# ended, a second job got the same work, and once the question timed out
# both pushed to one branch (remi#881, remi#892). A one-way question past
# its deadline is left for the owner: their answer is still owed, so
# nothing else starts on it either.
# So is one whose resume failed with its question still outstanding: its
# deadline resumes it, so it isn't a broken job to start again.
for end, held, said in (("done", "ask", "an answer to its question"), ("done", "owner", "Sam's answer to its one-way question"),
                        ("failed", "ask", "an answer to its question")):
    fresh_pr_state()
    run(clean(**threads), 0)
    run(clean(**threads), 7200)
    assert len(JOBS) == 1, JOBS
    ends(end)
    HELD[ISSUED[-1]] = [held]
    for t in (7300, 7400, 7400 + 10 * 3600):
        status, digest = a_pass(clean(**threads), t)
        assert JOBS == [] and f"its Claude job awaits {said}" in status and "state=pending" in status, (end, held, t, status, JOBS)
        assert digest == [] and overdue() is None and stall()["kick"] is None, (held, t, digest, waits.FAILING)
    # Answered (or, reversible, timed out): proj resumes that same job.
    STATES[ISSUED[-1]] = "running"
    del HELD[ISSUED[-1]]
    status, _ = run(clean(**threads), 7400 + 11 * 3600)
    assert JOBS == [] and "a Claude job has it since" in status, (held, status, JOBS)
    # Its turn over, holding nothing: it ended without fixing it.
    ends("done")
    run(clean(**threads), 7400 + 12 * 3600)
    status, _ = run(clean(**threads), 7400 + 13 * 3600)
    assert len(JOBS) == 1 and "awaits" not in status, (held, status, JOBS)
STATES.clear()

# A judge holding a question isn't lost either: no second judge, and the
# PR waits on its answer as on a hold. A new head stops it, as one at work.
fresh_pr_state()
run(clean(**gated), 700)
judge = ISSUED[-1]
ends("done")
HELD[judge] = ["ask"]
for t in (5000, 5100, 5100 + 10 * 3600):
    status, digest = a_pass(clean(**gated), t)
    assert JOBS == [] and "the Claude job judging it awaits an answer to its question" in status, (t, status, JOBS)
    assert digest == [] and overdue(6) is None and stall(6)["kick"] is None, (t, digest, stall(6))
STOPPED.clear()
run(clean(sha=newer, **gated), 5100 + 11 * 3600)
assert judge in STOPPED, STOPPED
STATES.clear()
HELD.clear()
print("jobs without their answer: ok")

# A proof job that stops before it finishes, however it stops: started again
# a minute after a second look sees it ended, then two, four, eight; the PR
# never fails for it. From the third its wait is overdue, and stays so while
# the next waits, runs and ends, until one finishes its turn.
S.CFG["repos"]["sam/r"] = {"proofJobs": {"remi-proof": "Run `just proof` and post the remi-proof status."}}
PROOF = CI_GREEN + [{"__typename": "StatusContext", "context": "remi-proof", "state": "PENDING", "description": ""}]
proof = clean(checks=PROOF)
RUNNING = "a Claude job is running remi-proof"
fresh_pr_state()
status, _ = a_pass(proof, 700)
assert RUNNING in status and len(JOBS) == 1, (status, JOBS)
# Its wait is on that Claude job, which the watchdog holds while Claude is
# out of usage; once the job has ended, or its step keeps failing, it's
# the steward's again (claude False below).
assert stall()["claude"] is True, stall()
t = 700
BROKE = (("timed out after 60m", "timed out after 60m"), ("went quiet for 20m", "went quiet for 20m"),
         ("failed", "crashed"), ("error", "ended in an error"))
for i, (end, said) in enumerate(BROKE):
    late = LOST if i >= 2 else None
    ends(end)
    status, _ = a_pass(proof, t + 100)  # the first look
    assert JOBS == [] and RUNNING in status and overdue() == (LOST if i >= 3 else None), (i, status, waits.FAILING)
    assert stall()["claude"] is False, (i, stall())
    one_stall(i)
    status, digest = a_pass(proof, t + 200)
    due = t + 200 + 2 ** i * 60
    assert JOBS == [] and f"the last Claude job running remi-proof {said}, so another starts at {H.clock(due + T)}" in status, \
        (i, status)
    assert "state=failure" not in status and pr_state()["strikes"] == {"lost": i + 1}, (i, status, pr_state())
    assert C.WAKE["sam/r"] == T + due + 1 and overdue() == late and digest == [], (i, C.WAKE, waits.FAILING, digest)
    one_stall(i)
    # A pass in between (a kick, say) books the retry's pass again: each
    # pass replaces the repo's wake-ups.
    status, _ = a_pass(proof, due - 30)
    assert JOBS == [] and "so another starts at" in status and overdue() == late, (i, status, waits.FAILING)
    assert C.WAKE["sam/r"] == T + due + 1, (i, C.WAKE)
    one_stall(i)
    status, digest = a_pass(proof, due)
    assert len(JOBS) == 1 and f"running remi-proof again (the last one {said})" in status and digest == [], (i, status)
    assert overdue() == late, (i, waits.FAILING)
    one_stall(i)
    status, _ = a_pass(proof, due + 50)
    assert JOBS == [] and RUNNING in status and overdue() == late, (i, status, waits.FAILING)
    assert stall()["claude"] is (late is None), (i, stall())
    one_stall(i)
    t = due + 50
assert STUCK, "the run's stall was watched"
# Without the watchdog, it's listed on the stuck issue each pass instead.
watch, S.CFG["watch"] = S.CFG["watch"], None
_, digest = a_pass(proof, t + 100)
S.CFG["watch"] = watch
assert digest == [("Failing again and again", "[sam/r#5](u) t", f"{LOST}, 4 times in a row (journalctl -u autoland)")], digest
# One that finishes its turn without its verdict ends the run, and counts
# toward the two that fail the PR: a broken one before it didn't.
ends("done")
a_pass(proof, t + 200)
status, digest = a_pass(proof, t + 300)
assert len(JOBS) == 1 and "running remi-proof again (the last one ended without its verdict)" in status, status
assert "lost" not in pr_state()["strikes"] and overdue() is None and digest == [], (pr_state(), waits.FAILING)
assert stall()["since"] != STUCK[0], stall()  # the run's stall is over
ends("done")
a_pass(proof, t + 400)
status, digest = a_pass(proof, t + 500)
assert "state=failure" in status and [x[0] for x in digest] == ["Two Claude jobs ended without its verdict"], (status, digest)
# A new head starts the count over.
status, _ = a_pass(clean(sha=newer, checks=PROOF), t + 600)
assert RUNNING in status and len(JOBS) == 1 and pr_state()["strikes"] == {}, (status, pr_state())

# One that broke after one that gave no verdict is still started again: the
# two that fail the PR are two answers missing, not two jobs.
fresh_pr_state()
a_pass(proof, 700)
ends("done")
a_pass(proof, 800)
assert "again (the last one ended without its verdict)" in a_pass(proof, 900)[0]
ends("timed out after 60m")
a_pass(proof, 1000)
status, _ = a_pass(proof, 1100)
assert "so another starts at" in status and "state=failure" not in status, status
status, _ = a_pass(proof, 1160)
assert len(JOBS) == 1 and "again (the last one timed out after 60m)" in status, status

# With proofSlots, one started in place of a broken one waits its turn like
# any other, even after one that gave no verdict; the broken one is counted,
# and its retry booked, while the slot is taken.
S.CFG["repos"]["sam/r"]["proofSlots"] = 1
fresh_pr_state()
P.begin("sam/r", {4, 5})
a_pass(proof, 700)
ends("done")
a_pass(proof, 800)
a_pass(proof, 900)
ends("timed out after 60m")
a_pass(proof, 1000)
P.begin("sam/r", {4, 5})
assert RUNNING in run(clean(checks=PROOF, number=4), 1050)[0]  # another PR took the slot meanwhile
status, _ = a_pass(proof, 1100)
assert JOBS == [] and "waiting its turn to run remi-proof (1 ahead)" in status, status
assert pr_state()["strikes"] == {"lost": 1} and C.WAKE["sam/r"] == T + 1161, (pr_state(), C.WAKE)
status, _ = a_pass(proof, 1160)
assert JOBS == [] and "waiting its turn to run remi-proof (1 ahead)" in status and pr_state()["strikes"] == {"lost": 1}, \
    (status, pr_state())
del S.CFG["repos"]["sam/r"]
P.OPEN.clear()
P.WAITING.clear()
STATES.clear()

# A judge and a fix job that break are started again the same way.
fresh_pr_state()
run(clean(**gated), 700)
ends("went quiet for 20m")
run(clean(**gated), 800)
status, _ = run(clean(**gated), 900)
assert JOBS == [] and "the last Claude job judging it went quiet for 20m, so another starts at" in status, status
status, _ = run(clean(**gated), 930)
assert JOBS == [] and "so another starts at" in status and C.WAKE["sam/r"] == T + 961, (status, C.WAKE)
status, _ = run(clean(**gated), 960)
assert len(JOBS) == 1 and "judging it again (the last one went quiet for 20m)" in status, status
fresh_pr_state()
run(clean(threads=[False]), 0)
run(clean(threads=[False]), 7200)
ends("timed out after 60m")
run(clean(threads=[False]), 7300)
status, _ = run(clean(threads=[False]), 7400)
assert JOBS == [] and "the last Claude job on it timed out after 60m, so another takes it at" in status, status
status, _ = run(clean(threads=[False]), 7430)
assert JOBS == [] and "so another takes it at" in status and C.WAKE["sam/r"] == T + 7461, (status, C.WAKE)
status, _ = run(clean(threads=[False]), 7460)
assert len(JOBS) == 1 and "a Claude job has it since" in status and "state=failure" not in status, status
assert pr_state().get("handedAgain") is None, pr_state()
STATES.clear()

C.job = kept
fresh_pr_state()
print("jobs that broke: ok")

# A wait is on Claude only while a Claude job it looked at is queued or at
# work: not once it has ended or proj can't say, nor for a proof build.
for jobs, claude in (({"job-1": "queued"}, True), ({"job-1": "running"}, True), ({"job-1": ""}, False),
                     ({"job-1": "unknown"}, False), ({C.BUILD + "1": "running"}, False), (None, False)):
    waits.begin()
    waits.read("sam/r", [clean()])
    waits.note("sam/r", clean(), "Settled", waits.STEWARD, jobs=jobs)
    assert waits.NOTED["sam/r"]["sam/r#5"]["claude"] is claude, (jobs, waits.NOTED)
waits.begin()
print("waits on Claude: ok")
