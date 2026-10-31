"""A PR whose changes conflict with its base: GitHub refuses the merge, won't
bring it up to date and runs no CI on it, so nothing is merged or asked of
GitHub, and a Claude job merges the base in at once, one a head on its base,
before any review or CI. Ordinary and judged PRs alike; a dependency bot's
own update is the bot's to rebase, and one renovate-sweep has is its job's."""

from harness import COMMITS, JOBS, STATES, WROTE, clean, fresh_pr_state, judgment, merged, merges, mkpr, pr_state, queued, \
    run, said, stand_in, swept
from touches import DEPENDABOT_EMAIL

import waits

stand_in()


def conflicting(pr):
    return {**pr, "mergeStateStatus": "DIRTY"}


def dirty(**kw):
    return conflicting(clean(**kw))


def desc(status):
    return status.partition("description=")[2].partition(" -f target_url")[0]


for pr in (clean(), clean(files=(".github/CODEOWNERS",), number=6, comments=[said(judgment("land"))]), clean(base="release")):
    at_odds = conflicting(pr)
    base = pr["baseRefName"]
    fresh_pr_state()
    status, digest = run(at_odds, 900)
    assert merges() == [] and not any("/update-branch" in x for x in WROTE), (pr["number"], WROTE)
    assert len(JOBS) == 1 and f"it conflicts with {base}" in JOBS[0] and f"merge {base} into the branch" in JOBS[0], JOBS
    # It checks the PR is still where it was handed before it merges or pushes.
    assert f"still open on {base} at head {pr['headRefOid']}: if its base or head moved on, stop" in JOBS[0], JOBS
    assert "state=pending" in status and f"Conflicts with {base}; a Claude job has it" in status, status
    assert digest == [], digest
    # One job a head while it works, however many passes see it.
    status, _ = run(at_odds, 960)
    assert JOBS == [] and merges() == [] and "a Claude job has it" in status, (JOBS, status)
    # It ended without pushing: once more, then the owner hears.
    STATES["job-1"] = "done"
    run(at_odds, 1000)
    status, _ = run(at_odds, 1060)
    assert len(JOBS) == 1 and "it conflicts with" in JOBS[0], JOBS
    run(at_odds, 1100)
    status, digest = run(at_odds, 1160)
    assert JOBS == [] and [x[0] for x in digest] == ["Two Claude jobs ended without fixing it"], (JOBS, digest)
    assert "state=failure" in status and merges() == [], status
    STATES.clear()

WAITS = "a Claude job takes it once the one on {} ends"
# Retargeted while its job is still on the old base: the job stays tracked
# (no second one beside it on the branch, its record kept) until it ends,
# then the new base gets its own, with two tries of its own: the old base's
# retry doesn't count against it.
fresh_pr_state()
run(dirty(), 900)
STATES["job-1"] = "done"
run(dirty(), 1000)
run(dirty(), 1060)
assert len(JOBS) == 1 and "merge main into" in JOBS[0], JOBS
# The whole gate can outlast the job on a loaded machine, and then the merge is
# never pushed: the job runs what the merge touched, and CI has the gate.
assert "tests of what the conflicts touched" in JOBS[0] and "CI runs the whole gate once you push" in JOBS[0] and "run the repo's tests" not in JOBS[0], JOBS
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
STATES["job-1"] = "done"
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

# A conflicted PR, settled otherwise: a Claude job merges its base in at
# once (GitHub won't merge it and runs no CI on it), and nothing merges it.
fresh_pr_state()
status, _ = run(conflicting(clean()), 700)
assert len(JOBS) == 1 and "it conflicts with main" in JOBS[0] and "never rebase or force-push" in JOBS[0], JOBS
assert desc(status).startswith("Conflicts with main; a Claude job has it since") and not merged(), (status, WROTE)
# While it works, nothing more starts; one that ends with the conflict still
# there is handed once more, and after a second the owner hears.
status, _ = run(conflicting(clean()), 800)
assert JOBS == [] and "a Claude job has it since" in status, (JOBS, status)
STATES["job-1"] = "done"
status, _ = run(conflicting(clean()), 900)
assert JOBS == [] and "its Claude job just ended" in status, (JOBS, status)
run(conflicting(clean()), 1000)
assert len(JOBS) == 1 and "it conflicts with main" in JOBS[0], JOBS
run(conflicting(clean()), 1100)
status, digest = run(conflicting(clean()), 1200)
assert "state=failure" in status and digest[0][0] == "Two Claude jobs ended without fixing it", (status, digest)
STATES.clear()
# Before any review or CI too: neither can settle it while it conflicts.
fresh_pr_state()
status, _ = run(conflicting(mkpr(checks=[])), 700)
assert len(JOBS) == 1 and queued() == [] and "Conflicts with main" in status, (JOBS, queued(), status)
# A bot's own update is the bot's to rebase.
fresh_pr_state()
status, _ = run(conflicting(clean(branch="renovate/foo-1.x", title="Update foo from 1.4 to 1.5")), 700)
assert JOBS == [] and "Conflicts with main; its bot rebases it" in status and not merged(), (JOBS, status)
# A Dependabot one too, and still open a day on, it's listed for the owner.
dependabot = {"branch": "dependabot/cargo/sqlx-0.8.7", "title": "Bump sqlx from 0.8.6 to 0.8.7", "number": 15}
COMMITS[15] = [DEPENDABOT_EMAIL]
fresh_pr_state()
status, digest = run(conflicting(clean(**dependabot)), 90000)
assert JOBS == [] and "its bot rebases it" in status, (JOBS, status)
assert digest == [("Open over a day", digest[0][1], "Dependency update; conflicts with main")], digest
# Only its own, though: with anyone else's commit on it, the bot leaves the
# branch alone, major or minor, Renovate's or Dependabot's, and a job of
# ours merges the base in.
major = {"branch": "renovate/foo-2.x", "title": "Update foo from 1.4 to 2.0", "number": 8}
COMMITS[8] = ["bot@renovateapp.com", "claude@remi"]
COMMITS[15] = [DEPENDABOT_EMAIL, "claude@remi"]
for pr in (clean(comments=[said(swept("ok"))], **major), clean(**dependabot)):
    fresh_pr_state()
    status, _ = run(conflicting(pr), 700)
    assert len(JOBS) == 1 and "it conflicts with main" in JOBS[0] and "a Claude job has it" in status, (JOBS, status)
# A conflict job stuck in line (or one whose state can't be read: running,
# as far as the steward can tell) doesn't keep the PR off the list: still
# open a day on, it's listed, watchdog or not, a Renovate update a job of
# ours works included.
for pr in (clean(), clean(comments=[said(swept("ok"))], **major)):
    fresh_pr_state()
    run(conflicting(pr), 700)
    status, digest = run(conflicting(pr), 90000)
    assert "a Claude job has it" in status and [d[0] for d in digest] == ["Open over a day"], (status, digest)
# One renovate-sweep has is its job's: a red one, or a major with no verdict
# on this head, gets no job of ours beside it on the branch.
red = [{"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "FAILURE"}]
for pr in (clean(checks=red, **major), clean(**major)):
    fresh_pr_state()
    status, _ = run(conflicting(pr), 700)
    assert JOBS == [] and "Conflicts with main; renovate-sweep has it" in status, (JOBS, status)
    assert waits.NOTED["sam/r"]["sam/r#8"]["kick"] == "renovate-sweep/sam/r", waits.NOTED
    # Its escalation is renovate-sweep's too: not listed here a day on.
    status, digest = run(conflicting(pr), 90000)
    assert "renovate-sweep has it" in status and digest == [], (status, digest)
# And one it held is the owner's: nothing pushes to it.
fresh_pr_state()
status, _ = run(conflicting(clean(comments=[said(swept("held"))], **major)), 700)
assert JOBS == [] and "renovate-sweep held it" in status, (JOBS, status)
COMMITS.clear()
fresh_pr_state()
print("conflicts: ok")
