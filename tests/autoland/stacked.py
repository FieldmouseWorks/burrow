"""A PR stacked on another open PR's branch: merged, it would land in that
branch, folded into the PR below. Until it's retargeted, nobody asks Codex
to review it, no Claude job works on it, nothing merges it and nobody waits on it; once
it's retargeted it's a PR like any other. A PR into a branch no open PR has
(a release branch) isn't stacked."""

import waits
from harness import JOBS, WROTE, clean, fresh_pr_state, merged, mkpr, run, summary

RED = [{"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "FAILURE"}]

# Settled and green on its base, threads open, or CI red: all the same.
for why, pr in [("settled", clean(base="feat-a", branch="feat-b", number=6)),
                ("fresh", mkpr(base="feat-a", branch="feat-b", number=6)),
                ("open threads", mkpr(base="feat-a", branch="feat-b", number=6, threads=(False,))),
                ("red", clean(base="feat-a", branch="feat-b", number=6, checks=RED))]:
    fresh_pr_state()
    for t in (0, 700, 2 * 86400):
        status, digest = run(pr, t, below=5)
        assert "state=pending" in status and "Stacked on #5 (feat-a)" in status, (why, status)
        asked = [x for x in WROTE if "@codex review" in x]
        assert not merged() and asked == [] and JOBS == [] and digest == [], (why, status, JOBS, digest)
        assert waits.NOTED["sam/r"]["sam/r#6"] is None, (why, waits.NOTED)

# Retargeted to the default branch, the PR below merged: reviewed there, then
# landed.
fresh_pr_state()
run(mkpr(base="feat-a", branch="feat-b", number=6), 0, below=5)
status, _ = run(mkpr(base="main", branch="feat-b", number=6), 700)
assert "Waiting for Codex's review of this commit" in status and not merged(), status
status, _ = run(mkpr(base="main", branch="feat-b", number=6, comments=[summary(at="2026-10-24T00:12:30Z")]), 800)
assert merged(), status

# Into a branch no open PR has: not stacked, so it lands there.
fresh_pr_state()
status, _ = run(clean(base="release/1.x", number=6), 700)
assert merged() and "Stacked" not in status, status

print("stacked PRs: ok")
