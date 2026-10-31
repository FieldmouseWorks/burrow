"""Past the review-round limit: the reviewers aren't asked again, a Claude
job settles what's left and a judge decides, while whatever look is still
owed on the head (on its way, lost in posting, or on another base) holds the
pass back."""

import os

from harness import (C, JOBS, POSTED, STUB, T0, V, WROTE, fresh_pr_state, head, judgment, merged, mkpr, pr_state,
                     queued, run, rv_review, said, stand_in, tmp, worker)

stand_in()
newer = "9" * 40

# Past the round limit the reviewers aren't asked again, and neither is the
# owner: a Claude job settles what's left on its merits, then a judge decides.
fresh_pr_state()
stuck = {"reviews": [rv_review("codex", 1)] * 5, "threads": [False]}
status, digest = run(mkpr(**stuck), 0)
assert not any("--add-label" in x or "/comments " in x for x in WROTE) and digest == [], (WROTE, digest)
status, digest = run(mkpr(**stuck), 7200)
assert not any("--add-label" in x or "/comments " in x for x in WROTE) and digest == [], (WROTE, digest)
assert queued() == [] and "Past the review-round limit (5): 1 open review thread" in status, (queued(), status)
assert len(JOBS) == 1 and "won't look again" in JOBS[0] and "answer the rest with why" in JOBS[0], JOBS
# Answered without a push: still no look, a judge instead, told to read the answers too.
answered = {"reviews": stuck["reviews"], "threads": [True]}
status, digest = run(mkpr(**answered), 7300)
assert queued() == [] and len(JOBS) == 1 and "past its review-round limit" in JOBS[0], (queued(), JOBS)
assert "a finding answered away" in JOBS[0] and "found nothing" not in JOBS[0], JOBS
assert "a Claude job is judging it (it went past the review-round limit" in status and not merged() and digest == [], status
status, _ = run(mkpr(comments=[said(judgment("land", n=5))], **answered), 7400)
assert merged() and "judged it fine to land" in status, status
# A push past the limit isn't looked at either: the judge reads it, and a
# finding it says was answered away for real goes to a fix job.
fresh_pr_state()
pushed = {**answered, "sha": newer}
status, _ = run(mkpr(**pushed), 0)
assert queued() == [] and len(JOBS) == 1 and "past its review-round limit" in JOBS[0], (queued(), JOBS)
status, _ = run(mkpr(comments=[said(judgment("failed", n=5, sha=newer))], **pushed), 7300)
assert not merged() and len(JOBS) == 1 and "a finding left unfixed" in JOBS[0], (status, JOBS)
# The owner hears only if the judge sends it to them.
status, digest = run(mkpr(comments=[said(judgment("owner", n=5, sha=newer))], **pushed), 7400)
assert not merged() and [x[0] for x in digest] == ["Judged yours to decide"], digest
# A clean look that comes back on the head anyway is a clean head like any other.
fresh_pr_state()
status, _ = run(mkpr(reviews=[*stuck["reviews"], rv_review("codex", 0), rv_review("rival", 0)], threads=[True]), 700)
assert merged() and JOBS == [], (status, JOBS)

# A look already on its way when the limit's reached is waited for, and what
# it finds holds the pass back, whatever the judge said.
fresh_pr_state()
four = {"reviews": [rv_review("codex", 1)] * 4, "threads": [True]}
five = {"reviews": [rv_review("codex", 1)] * 5, "threads": [True]}
run(mkpr(**four), 0)
run(mkpr(**four), 600)
assert queued() == ["rival"], queued()
status, _ = run(mkpr(**five), 700)
assert JOBS == [] and not merged() and "Rival is reviewing this commit" in status, (status, JOBS)
worker(rival={"result": "review", "answer": {"findings": [{"path": "a.py"}]}})
status, _ = run(mkpr(comments=[said(judgment("land", n=5))], **five), 800)
assert not merged() and f"Past the review-round limit (5): review findings on {head[:7]}" in status, status
# Findings that couldn't be posted aren't dropped for want of another look:
# that one's asked again, the same round.
fresh_pr_state()
run(mkpr(**four), 0)
run(mkpr(**four), 600)
worker(rival={"result": "review", "answer": {"findings": [{"path": "a.py"}]}})
STUB["post"] = RuntimeError("HTTP 502")
for t in (650, 710, 770):
    run(mkpr(**five), t)
STUB["post"] = None
assert pr_state()["lostOn"]["rival"] == {"main": head}, pr_state()
status, _ = run(mkpr(comments=[said(judgment("land", n=5))], **five), 830)
assert not merged() and JOBS == [] and "Waiting for Rival's review" in status, (status, JOBS)
status, _ = run(mkpr(comments=[said(judgment("land", n=5))], **five), 770 + 3700)
assert queued() == ["rival"] and not merged() and JOBS == [], (queued(), status, JOBS)
# So is a look at this head after findings on it, or at an earlier head:
# what's still in the queue for the PR, begun or not, is waited for.
for at, pushed_to in ((head, head), (head, newer)):
    fresh_pr_state()
    V.queue("sam/r", mkpr(sha=at), "pr-sam_r-5.json", "codex")
    status, _ = run(mkpr(sha=pushed_to, **five), 700)
    assert JOBS == [] and not merged() and "Waiting for Codex's review" in status, (pushed_to, status, JOBS)
    worker(codex={"result": "review", "answer": {"findings": [{"path": "a.py"}]}})
    status, _ = run(mkpr(sha=pushed_to, comments=[said(judgment("land", n=5, sha=pushed_to))], **five), 800)
    assert not merged() and f"review findings on {at[:7]}" in status, (pushed_to, status)
# Findings lost after earlier ones on the head, then a push: still owed, and
# asked for on the new head.
fresh_pr_state()
V.queue("sam/r", mkpr(), "pr-sam_r-5.json", "codex")
worker(codex={"result": "review", "answer": {"findings": [{"path": "a.py"}]}})
STUB["post"] = RuntimeError("HTTP 502")
for t in (650, 710, 770):
    run(mkpr(**five), t)
STUB["post"] = None
landed_newer = {**five, "sha": newer, "comments": [said(judgment("land", n=5, sha=newer))]}
status, _ = run(mkpr(**landed_newer), 800)
assert not merged() and JOBS == [] and "Waiting for Codex's review" in status, (status, JOBS)
status, _ = run(mkpr(**landed_newer), 1500)
assert queued() == ["codex"] and not merged(), (queued(), status)
worker()
status, _ = run(mkpr(reviews=[*five["reviews"], rv_review("codex", 0, newer)], threads=[True], sha=newer,
                     comments=landed_newer["comments"]), 1600)
assert "lostOn" not in pr_state() or not pr_state()["lostOn"], pr_state()
# A look at an earlier head posting in the pass that loses one at this head
# doesn't cover the lost one.
fresh_pr_state()
V.queue("sam/r", mkpr(sha=newer), "pr-sam_r-5.json", "codex")
worker(codex={"result": "review", "answer": {"findings": [{"path": "a.py"}]}})
real_post = V.post


def post_but_newer(repo, pr, r, answer, description):
    if pr["headRefOid"] == newer:
        raise RuntimeError("HTTP 502")
    return real_post(repo, pr, r, answer, description)


V.post = post_but_newer
for t in (650, 710):
    run(mkpr(sha=newer, **five), t)
V.queue("sam/r", mkpr(), "pr-sam_r-5.json", "codex")
worker()
run(mkpr(sha=newer, **five), 770)
V.post = real_post
assert ("codex", head, 0) in POSTED, POSTED
assert pr_state()["lostOn"]["codex"] == {"main": newer}, pr_state()
status, _ = run(mkpr(**landed_newer), 830)
assert not merged() and JOBS == [] and "Waiting for Codex's review" in status, (status, JOBS)
# A loss recorded before lostOn, in its head's rv, still holds after a push.
fresh_pr_state()
C.save("pr-sam_r-5.json", {"head": head, "seen": C.ts(T0), "rv": {"codex": {"lost": [head, "main"]}}})
status, _ = run(mkpr(**landed_newer), 700)
assert not merged() and JOBS == [] and "Waiting for Codex's review" in status, (status, JOBS)
assert pr_state()["lostOn"] == {"codex": {"main": head}} and "lost" not in pr_state()["rv"].get("codex", {}), pr_state()
# Nor does a look at the PR on another base: retargeted and back, the loss
# on this one still holds.
fresh_pr_state()
V.queue("sam/r", mkpr(), "pr-sam_r-5.json", "codex")
worker(codex={"result": "review", "answer": {"findings": [{"path": "a.py"}]}})
STUB["post"] = RuntimeError("HTTP 502")
for t in (650, 710, 770):
    run(mkpr(**five), t)
STUB["post"] = None
V.queue("sam/r", mkpr(base="release/1.x"), "pr-sam_r-5.json", "codex")
worker()
run(mkpr(base="release/1.x", **five), 800)
assert ("codex", head, 0) in POSTED and pr_state()["lostOn"] == {"codex": {"main": head}}, (POSTED, pr_state())
status, _ = run(mkpr(comments=[said(judgment("land", n=5))], **five), 830)
assert not merged() and JOBS == [] and "Codex is reviewing this commit" in status, (status, JOBS)
# Nor are findings from a look on a base the PR has since left: owed there.
fresh_pr_state()
V.queue("sam/r", mkpr(base="release/1.x"), "pr-sam_r-5.json", "codex")
worker(codex={"result": "review", "answer": {"findings": [{"path": "a.py"}]}})
run(mkpr(**five), 700)
assert POSTED == [] and pr_state()["lostOn"] == {"codex": {"release/1.x": head}}, (POSTED, pr_state())
status, _ = run(mkpr(base="release/1.x", comments=[said(judgment("land", n=5, base="release/1.x"))], **five), 800)
assert not merged() and JOBS == [] and "Codex" in status, (status, JOBS)
# The limit holds while the reviewer that reached it is out of service, and
# with none in service the judge still decides.
for out in (["codex"], ["codex", "rival"]):
    fresh_pr_state()
    C.save(V.OUT, {r: {"why": "usage", "until": C.ts(T0) + 86400} for r in out})
    status, digest = run(mkpr(**five), 700)
    assert queued() == [] and len(JOBS) == 1 and "past its review-round limit" in JOBS[0], (out, queued(), status, JOBS)
    assert "No reviewer can review it" not in [x[0] for x in digest], digest
    os.remove(f"{tmp}/state/{V.OUT}")
print("rounds: ok")
