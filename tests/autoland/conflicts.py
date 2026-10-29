"""A settled PR whose changes conflict with its base: GitHub refuses the merge
and won't bring it up to date, so nothing is merged or asked of GitHub, and a
Claude job merges the base in, one a head on its base. Ordinary and judged
PRs alike."""

from harness import JOBS, STATES, WROTE, clean, fresh_pr_state, judgment, merges, run, said

for pr in (clean(), clean(files=(".github/CODEOWNERS",), number=6, comments=[said(judgment("land"))]), clean(base="release")):
    dirty = {**pr, "mergeStateStatus": "DIRTY"}
    base = pr["baseRefName"]
    fresh_pr_state()
    status, digest = run(dirty, 900)
    assert merges() == [] and not any("/update-branch" in x for x in WROTE), (pr["number"], WROTE)
    assert len(JOBS) == 1 and f"it conflicts with {base}" in JOBS[0] and f"merge {base} into the branch" in JOBS[0], JOBS
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
fresh_pr_state()
print("conflicts: ok")
