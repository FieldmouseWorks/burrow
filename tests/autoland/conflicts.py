"""A settled PR whose changes conflict with its base: GitHub refuses the merge
and won't bring it up to date, so nothing is merged or asked of GitHub, and a
Claude job merges the base in, one a head on its base. Ordinary and judged
PRs alike."""

from harness import JOBS, STATES, WROTE, clean, fresh_pr_state, judgment, merges, pr_state, run, said

for pr in (clean(), clean(files=(".github/CODEOWNERS",), number=6, comments=[said(judgment("land"))]), clean(base="release")):
    dirty = {**pr, "mergeStateStatus": "DIRTY"}
    base = pr["baseRefName"]
    fresh_pr_state()
    status, digest = run(dirty, 900)
    assert merges() == [] and not any("/update-branch" in x for x in WROTE), (pr["number"], WROTE)
    assert len(JOBS) == 1 and f"it conflicts with {base}" in JOBS[0] and f"merge {base} into the branch" in JOBS[0], JOBS
    # It checks the PR is still where it was handed before it merges or pushes.
    assert f"still open on {base} at head {pr['headRefOid']}: if its base or head moved on, stop" in JOBS[0], JOBS
    assert "state=pending" in status and f"Settled, but it conflicts with {base}; a Claude job has it" in status, status
    assert digest == [], digest
    # One job a head while it works, however many passes see it.
    status, _ = run(dirty, 960)
    assert JOBS == [] and merges() == [] and "a Claude job has it" in status, (JOBS, status)
    # It ended without pushing: once more, then the owner hears.
    STATES["job-1"] = "error"
    run(dirty, 1000)
    status, _ = run(dirty, 1060)
    assert len(JOBS) == 1 and "it conflicts with" in JOBS[0], JOBS
    run(dirty, 1100)
    status, digest = run(dirty, 1160)
    assert JOBS == [] and [x[0] for x in digest] == ["Two Claude jobs ended without fixing it"], (JOBS, digest)
    assert "state=failure" in status and merges() == [], status
    STATES.clear()


def dirty(**kw):
    return {**clean(**kw), "mergeStateStatus": "DIRTY"}


WAITS = "a Claude job takes it once the one on {} ends"
# Retargeted while its job is still on the old base: the job stays tracked
# (no second one beside it on the branch, its record kept) until it ends,
# then the new base gets its own, with two tries of its own: the old base's
# retry doesn't count against it.
fresh_pr_state()
run(dirty(), 900)
STATES["job-1"] = "error"
run(dirty(), 1000)
run(dirty(), 1060)
assert len(JOBS) == 1 and "merge main into" in JOBS[0], JOBS
STATES["job-1"] = "running"  # the second one (ids restart each pass here)
for state in ("running", "queued", None):
    STATES["job-1"] = state
    status, digest = run(dirty(base="release"), 1100)
    assert JOBS == [] and WAITS.format("its conflicts with its base") in status and digest == [], (state, JOBS, status)
    assert pr_state()["conflictFixed"].endswith(" main") and pr_state()["conflictFixedJob"] == "job-1", pr_state()
STATES["job-1"] = "done"
status, _ = run(dirty(base="release"), 1200)
assert len(JOBS) == 1 and "merge release into" in JOBS[0] and "still open on release" in JOBS[0], JOBS
assert "a Claude job has it since" in status and "state=failure" not in status, status
STATES["job-1"] = "error"
run(dirty(base="release"), 1300)
status, _ = run(dirty(base="release"), 1360)
assert len(JOBS) == 1 and "merge release into" in JOBS[0] and "state=failure" not in status, (JOBS, status)
STATES.clear()

# A new head while a fix job of any kind is still at work on the old one:
# the next job, of its kind or another, waits for it to end.
new = "b" * 40
red = [{"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "FAILURE"}]
for first, then, on in ((dirty(), dirty(sha=new), "its conflicts with its base"),
                        (dirty(), clean(sha=new, checks=red), "its conflicts with its base"),
                        (clean(threads=[False]), dirty(sha=new), "its findings")):
    fresh_pr_state()
    started = [len(run(first, t) and JOBS) for t in (0, 7200)]
    assert sum(started) == 1, (on, started)
    run(then, 7300)
    status, _ = run(then, 7300 + 7200)
    assert JOBS == [] and WAITS.format(on) in status, (on, JOBS, status)
    STATES["job-1"] = "done"
    status, _ = run(then, 7300 + 7300)
    assert len(JOBS) == 1 and "a Claude job has it since" in status, (on, JOBS, status)
    STATES.clear()
fresh_pr_state()
print("conflicts: ok")
