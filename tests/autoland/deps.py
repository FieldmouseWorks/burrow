"""handle() on dependency updates: a Renovate major waits for
renovate-sweep's sealed verdict on its head, a minor or a security fix
merges itself, a Dependabot major is judged, and whose each one's threads
and red CI are."""

from harness import COMMITS, JOBS, S, T0, clean, fresh_pr_state, head, judgment, merged, mkpr, run, said, swept

other = "c" * 40  # another head
busy = [{"__typename": "CheckRun", "name": "build", "status": "IN_PROGRESS", "conclusion": None}]

# A Renovate major lands on renovate-sweep's ok for its head, from our login.
fresh_pr_state()
major = {"branch": "renovate/foo-2.x", "title": "Update foo from 1.4 to 2.0", "number": 8}
status, _ = run(mkpr(**major), 700)
assert not merged() and "waiting for renovate-sweep" in status and JOBS == []
# Every Claude job posts as our login, so one without major-seal's seal (a
# job quoting PR text), with another verdict's, PR's or base's, or under
# another key doesn't count either.
okmac = swept("ok").split()[-2]
for bad in (said(swept("ok"), who="mallory"), said(swept("ok", sha=other)), said(swept("held")),
            said(f"<!-- major-verdict: ok {head} -->"), said(swept("ok", base="release")),
            said(f"<!-- major-verdict: ok {head} on main -->"), said(swept("held").replace(swept("held").split()[-2], okmac)),
            said(swept("ok", n=9)), said(swept("ok").replace(okmac, "0" * 32))):
    status, _ = run(mkpr(comments=[bad], **major), 800)
    assert not merged() and JOBS == [], bad
# With no renovate-sweep on the machine (no key), none counts.
key, S.CFG["majorKey"] = S.CFG["majorKey"], None
status, _ = run(mkpr(comments=[said(swept("ok"))], **major), 850)
S.CFG["majorKey"] = key
assert not merged() and "waiting for renovate-sweep" in status, status
status, _ = run(mkpr(comments=[said(swept("ok"))], **major), 900)
assert "state=success" in status and merged()
# Retargeted since: the ok was for another base's diff, and counts no more.
fresh_pr_state()
status, _ = run(mkpr(comments=[said(swept("ok"))], base="release", **major), 900)
assert not merged() and "waiting for renovate-sweep" in status, status
# Its CI not green yet: the ok waits.
fresh_pr_state()
status, _ = run(mkpr(comments=[said(swept("ok"))], checks=busy, **major), 900)
assert not merged()
# One touching a one-way path still needs the ok: a judge's land doesn't
# replace a held or missing verdict.
fresh_pr_state()
gated = {**major, "files": ("secrets/app.yaml",)}
for said_ in ([], [said(swept("held"))]):
    status, _ = run(mkpr(comments=[*said_, said(judgment("land", n=8))], **gated), 900)
    assert not merged() and JOBS == [] and "renovate-sweep" in status, said_
status, _ = run(mkpr(comments=[said(swept("ok"))], **gated), 900)
assert not merged() and len(JOBS) == 1 and "secrets/" in JOBS[0]
assert "no reviewer reads" in JOBS[0] and "renovate-sweep cleared the major version" in JOBS[0], JOBS[0]
assert "its reviewers found nothing" not in JOBS[0], JOBS[0]
status, _ = run(mkpr(comments=[said(swept("ok")),
                               said(judgment("land", n=8))], **gated), 1000)
assert merged()
# A Renovate minor: Renovate merges it itself. A Dependabot major is judged.
fresh_pr_state()
status, _ = run(mkpr(branch="renovate/foo-1.x", title="Update foo from 1.4 to 1.5", number=9), 900)
assert "state=success" in status and not merged() and JOBS == []
status, _ = run(mkpr(branch="dependabot/npm/foo-2", title="Bump foo from 1.4 to 2.0", number=10), 900)
assert len(JOBS) == 1 and "a major dependency update" in JOBS[0] and not merged()
assert "no reviewer reads" in JOBS[0] and "its reviewers found nothing" not in JOBS[0], JOBS[0]

# A finding on a dependency update goes to a Claude job once it has sat, like
# any PR's: nobody else settles a green Renovate PR's threads.
fresh_pr_state()
minor = {"branch": "renovate/foo-1.x", "title": "Update foo from 1.4 to 1.5", "number": 12, "threads": [False]}
status, _ = run(mkpr(**minor), 0)
assert JOBS == [] and "1 open review thread" in status, status
status, _ = run(mkpr(**minor), 7200)
assert len(JOBS) == 1 and "1 open review thread" in JOBS[0] and not merged(), (status, JOBS)
status, _ = run(mkpr(**minor), 9000)
assert JOBS == [], "one job a head"
# A Renovate major's threads are renovate-sweep's until it gives the head a
# verdict; after that, nobody else's.
fresh_pr_state()
run(mkpr(threads=[False], **major), 0)
status, _ = run(mkpr(threads=[False], **major), 7200)
assert JOBS == [] and "renovate-sweep has it" in status, status
status, _ = run(mkpr(threads=[False], comments=[said(swept("ok"))], **major), 9000)
assert len(JOBS) == 1 and "1 open review thread" in JOBS[0] and not merged(), (status, JOBS)
# One it held is the owner's, threads and all: no job pushes to it.
fresh_pr_state()
run(mkpr(threads=[False], comments=[said(swept("held"))], **major), 0)
status, _ = run(mkpr(threads=[False], comments=[said(swept("held"))], **major), 9000)
assert JOBS == [] and not merged() and "renovate-sweep held it" in status, (status, JOBS)
# A red Renovate one is renovate-sweep's, threads and all: no second job on its branch.
fresh_pr_state()
red_ci = [{"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "FAILURE"}]
run(mkpr(checks=red_ci, **minor), 0)
status, _ = run(mkpr(checks=red_ci, **minor), 7200)
assert JOBS == [] and "renovate-sweep has it" in status, status
# A red Dependabot one has no sweep: it goes to a job.
fresh_pr_state()
bump = {"branch": "dependabot/npm/foo-1", "title": "Bump foo from 1.4 to 1.5", "number": 13}
run(mkpr(checks=red_ci, **bump), 0)
status, _ = run(mkpr(checks=red_ci, **bump), 7200)
assert len(JOBS) == 1 and "CI is red (ci)" in JOBS[0], (status, JOBS)
# One sent to the owner is theirs, whatever else is open on it: no job.
fresh_pr_state()
run(mkpr(checks=red_ci, labels=("needs-owner",), **bump), 0)
status, digest = run(mkpr(checks=red_ci, labels=("needs-owner",), **bump), 7200)
assert JOBS == [] and [x[0] for x in digest] == ["Labelled needs-owner"], (status, JOBS, digest)
# Changes requested on one go to a job like a thread; on a Renovate major
# without a verdict, to renovate-sweep.
pat = {"author": {"login": "pat"}, "state": "CHANGES_REQUESTED", "submittedAt": T0, "commit": {"oid": head}}
fresh_pr_state()
run(mkpr(reviews=[pat], **bump), 0)
status, _ = run(mkpr(reviews=[pat], **bump), 7200)
assert len(JOBS) == 1 and "changes requested by pat" in JOBS[0], (status, JOBS)
fresh_pr_state()
run(mkpr(reviews=[pat], **major), 0)
status, _ = run(mkpr(reviews=[pat], **major), 7200)
assert JOBS == [] and "changes requested by pat; renovate-sweep has it" in status, status

# A Renovate security fix merges itself, a major one too: no verdict waited on.
fresh_pr_state()
status, _ = run(mkpr(labels=("security",), **major), 900)
assert "state=success" in status and not merged() and JOBS == [], status

# A Renovate minor a Claude job fixed won't merge itself: it's reviewed and
# landed like a Claude PR.
fresh_pr_state()
COMMITS[14] = ["bot@renovateapp.com", "claude@remi"]
edited = {"branch": "renovate/foo-1.x", "title": "Update foo from 1.4 to 1.5", "number": 14}
status, _ = run(clean(**edited), 900)
assert merged(), status
# Red again, findings and all, it's renovate-sweep's: no job of ours on its branch.
fresh_pr_state()
run(mkpr(checks=red_ci, threads=[False], **edited), 0)
status, _ = run(mkpr(checks=red_ci, threads=[False], **edited), 7200)
assert JOBS == [] and not merged(), (status, JOBS)
status, _ = run(clean(checks=red_ci, **edited), 7200)
assert JOBS == [] and "CI is red" in status, (status, JOBS)

print("deps: ok")
