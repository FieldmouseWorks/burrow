"""Past the review-round limit: Codex isn't asked or waited for again, a
Claude job settles what's left and a judge decides."""

from harness import JOBS, WROTE, found, fresh_pr_state, judgment, merged, mkpr, run, said, stand_in, summary
import tasks as T

stand_in()
newer = "9" * 40

# Past the round limit the reviewers aren't asked again, and neither is the
# owner: a Claude job settles what's left on its merits, then a judge decides.
fresh_pr_state()
stuck = {"reviews": [found()] * 5, "threads": [False]}
status, digest = run(mkpr(**stuck), 0)
assert not any("--add-label needs-owner" in x or "/comments " in x for x in WROTE) and digest == [], (WROTE, digest)
status, digest = run(mkpr(**stuck), 7200)
assert not any("--add-label needs-owner" in x or "/comments " in x for x in WROTE) and digest == [], (WROTE, digest)
assert "Past the review-round limit (5): 1 open review thread" in status, status
assert len(JOBS) == 1 and "won't look again" in JOBS[0], JOBS
# Past the limit the job is held to the same policy as round one's (tasks.FINDINGS),
# stated once: no second, looser instruction beside it.
assert JOBS[0].count(T.FINDINGS) == 1, JOBS
assert JOBS[0].count("on its merits") + JOBS[0].count("on their merits") == 1, JOBS
assert "fix what's a real defect" not in JOBS[0], JOBS
# Answered without a push: still no look, a judge instead, told to read the answers too.
answered = {"reviews": stuck["reviews"], "threads": [True]}
status, digest = run(mkpr(**answered), 7300)
assert len(JOBS) == 1 and "past its review-round limit" in JOBS[0], JOBS
assert "a finding answered away" in JOBS[0] and "found nothing" not in JOBS[0], JOBS
# The judge counts an answer as failing by the same measure the fix job fixed by.
assert T.PAST_LIMIT in JOBS[0] and f"a finding answered away that is {T.MUST_FIX} counts as failing" in JOBS[0], JOBS
assert "a Claude job is judging it (it went past the review-round limit" in status and not merged() and digest == [], status
status, _ = run(mkpr(comments=[said(judgment("land", n=5))], **answered), 7400)
assert merged() and "judged it fine to land" in status, status
# A push past the limit isn't looked at either: the judge reads it, and a
# finding it says was answered away for real goes to a fix job.
fresh_pr_state()
pushed = {**answered, "sha": newer}
status, _ = run(mkpr(**pushed), 0)
assert len(JOBS) == 1 and "past its review-round limit" in JOBS[0], JOBS
status, _ = run(mkpr(comments=[said(judgment("failed", n=5, sha=newer))], **pushed), 7300)
assert not merged() and len(JOBS) == 1 and "a finding left unfixed" in JOBS[0], (status, JOBS)
# The owner hears only if the judge sends it to them.
status, digest = run(mkpr(comments=[said(judgment("owner", n=5, sha=newer))], **pushed), 7400)
assert not merged() and [x[0] for x in digest] == ["Judged yours to decide"], digest
# A clean look that comes back on the head anyway is a clean head like any other.
fresh_pr_state()
status, _ = run(mkpr(reviews=stuck["reviews"], comments=[summary(at="2026-10-24T00:12:00Z")], threads=[True]), 700)
assert merged() and JOBS == [], (status, JOBS)

print("rounds: ok")
