"""A judge's failed verdict says what fails: the code, which a fix job's
push settles, or the description alone, which a fix job settles by editing
it and pushing nothing, since a push starts the head's CI and proofs over
(burrow#507). A failed line from before the two were told apart is the
code's."""

from harness import (CI_GREEN, JOBS, STATES, clean, fresh_pr_state, head, judgment, merged, run, said, stand_in)

stand_in()
gated = {"files": ["secrets/app.yaml"], "number": 6}

# The judge is given both lines, sealed, and told which is which.
fresh_pr_state()
run(clean(**gated), 700)
assert len(JOBS) == 1 and f"autoland-judgment: failed {head}" in JOBS[0], JOBS
assert f"description is wrong about it, <!-- autoland-judgment: misdescribed {head}" in JOBS[0], JOBS[0]

# Only its description is wrong: a fix job that edits it and pushes nothing,
# and a status that says so.
fresh_pr_state()
wrong = said(f"The code's fine, but the description says the guard is unchanged.\n\n{judgment('misdescribed')}")
run(clean(**gated), 700)
status, digest = run(clean(comments=[wrong], **gated), 800)
assert len(JOBS) == 1 and "isn't settled" in JOBS[0] and "its description wrong about this head" in JOBS[0], JOBS
task = JOBS[0]
assert "Push nothing: no commit, no merge or rebase onto its base" in task, task
assert "write the description anew to match this head" in task and '"description": TEXT' in task, task
assert "git push" not in task and "If CI is red" not in task and "Merge main into the branch" not in task, task
assert "Settled, but its judge found its description wrong; a Claude job has it since" in status, status
assert "build or tests fail" not in status and not merged() and digest == [], (status, digest)
# Once the edited description is up, the same head is judged again: no push needed.
STATES["job-1"] = "done"
status, _ = run(clean(comments=[wrong], body="It changes the guard's undo snapshot.", **gated), 900)
assert len(JOBS) == 1 and "so you judge it" in JOBS[0] and f"head {head}" in JOBS[0], (status, JOBS)
assert JOBS[0].endswith("\n\nIts description:\n> It changes the guard's undo snapshot."), JOBS[0]
STATES.clear()

# The code fails (or a verdict line from before the kinds): today's fix job,
# which pushes its fix.
fresh_pr_state()
failed = said(f"`cargo test` fails.\n\n{judgment('failed')}")
run(clean(**gated), 700)
status, _ = run(clean(comments=[failed], **gated), 800)
assert len(JOBS) == 1 and "something failing" in JOBS[0] and "If CI is red" in JOBS[0], JOBS
assert "git push origin HEAD:refs/heads/feat" in JOBS[0] and "Push nothing" not in JOBS[0], JOBS[0]
assert "Settled, but its build or tests fail; a Claude job has it since" in status and not merged(), status

# Either holds the PR whatever CI is still doing: only a land waits for CI.
busy_ci = [{"__typename": "CheckRun", "name": "ci", "status": "IN_PROGRESS", "conclusion": None}]
fresh_pr_state()
run(clean(checks=[]), 0)
run(clean(checks=[]), 700)
status, _ = run(clean(checks=busy_ci, comments=[said(judgment("misdescribed", n=5))]), 800)
assert len(JOBS) == 1 and "Push nothing" in JOBS[0] and "description wrong" in status, (status, JOBS)
status, _ = run(clean(checks=CI_GREEN, comments=[said(judgment("misdescribed", n=5))]), 900)
assert not merged(), status
