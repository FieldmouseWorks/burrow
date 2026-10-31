"""A PR's waiting label (prs.show_waiting): put on by the step whose status
says what it waits on (its updates too new to take yet, its turn for a proof
slot, a reviewer back in service), the steward's others taken off in the
same call, and taken off once it waits on none of them. A PR whose labels are
already right costs no call, and one GitHub didn't take is asked for again
by the next pass."""

import itertools

import proofs as P
from harness import CI_GREEN, FAIL, JOBS, WROTE, C, S, T0, V, clean, fresh_pr_state, mkpr, run, said

AGE, SLOT, REVIEWER = "waiting: release age", "waiting: proof slot", "waiting: reviewer"


def edits():
    """The label edits this pass wrote."""
    return [x for x in WROTE if x.startswith("pr edit ")]


def on(n, *labels):
    return f"pr edit {n} -R sam/r " + " ".join(f"--add-label {x}" for x in labels)


# A lock file refresh nothing in which is old enough yet: its status says
# until when, and its label says why at a glance.
fresh_pr_state()
lock = {"branch": "renovate/cargo-lock-file-maintenance", "title": "Lock file maintenance (cargo)", "number": 16}
artifacts = {"__typename": "StatusContext", "context": "renovate/artifacts", "state": "FAILURE"}
refused = said("### ⚠️ Artifact update problem\n\nRenovate failed to update an artifact related to this branch.\n\n"
               "##### File name: undefined\n\n```\nCommand failed: /nix/store/x-lock-age/bin/lock-age main\n"
               "lock-age: Cargo.lock: ctutils 0.4.7 is too new, took 0.4.6\n"
               "lock-age: nothing in the refresh is old enough until 2026-10-27T00:03:00Z\n```\n\n")
status, _ = run(mkpr(checks=[*CI_GREEN, artifacts], comments=[refused], **lock), 7200)
assert "nothing in it old enough until" in status and edits() == [on(16, AGE)], (status, WROTE)
# Already on: no call.
run(mkpr(checks=[*CI_GREEN, artifacts], comments=[refused], labels=[AGE], **lock), 7300)
assert edits() == [], WROTE
# Refreshed with releases old enough, it waits on none of them: off.
status, _ = run(mkpr(labels=[AGE], **lock), 7400)
assert "nothing in it old enough" not in status and edits() == ["pr edit 16 -R sam/r --remove-label waiting: release age"], WROTE
# Red for any other reason, it's renovate-sweep's red CI, which no label names.
status, _ = run(mkpr(checks=[*CI_GREEN, artifacts], comments=[said("Not now.")], **lock), 7500)
assert "CI red; renovate-sweep has it" in status and edits() == [], (status, WROTE)

# Waiting its turn for a proof slot, and not once the turn has come.
kept, repos = C.job, dict(S.CFG["repos"])
ids = itertools.count(1)
C.job = lambda repo, task, clone=False, since=None: JOBS.append(task) or f"job-{next(ids)}"
S.CFG["repos"]["sam/r"] = {"proofJobs": {"remi-proof": "Run `just proof` and post the remi-proof status."}, "proofSlots": 1}
proof = CI_GREEN + [{"__typename": "StatusContext", "context": "remi-proof", "state": "PENDING", "description": ""}]
fresh_pr_state()
P.begin("sam/r", {1, 2})
status, _ = run(clean(checks=proof, number=1), 700)
assert "a Claude job is running remi-proof" in status and edits() == [], (status, WROTE)
status, _ = run(clean(checks=proof, number=2), 700)
assert "waiting its turn to run remi-proof (1 ahead)" in status and edits() == [on(2, SLOT)], (status, WROTE)
# Moved from one wait to another, the label goes with it in one call: put
# back for a reviewer out of service with its proof slot's still on.
C.save(V.OUT, {r: {"why": "spent", "until": C.ts(T0) + 86400} for r in V.NAMES})
status, _ = run(mkpr(checks=proof, number=2, labels=[SLOT]), 800)
assert "No reviewer can review it now" in status, status
assert edits() == ["pr edit 2 -R sam/r --add-label waiting: reviewer --remove-label waiting: proof slot"], WROTE
C.save(V.OUT, {})
C.job = kept
S.CFG["repos"] = repos

# Every reviewer out of service, it waits for one back. GitHub didn't take
# the label: the status stands all the same, and the next pass asks again.
fresh_pr_state()
C.save(V.OUT, {r: {"why": "spent", "until": C.ts(T0) + 86400} for r in V.NAMES})
FAIL.append("--add-label waiting: reviewer")
status, _ = run(clean(), 600)
assert "No reviewer can review it now" in status and edits() == [on(5, REVIEWER)], (status, WROTE)
FAIL.remove("--add-label waiting: reviewer")
status, _ = run(clean(), 700)
assert edits() == [on(5, REVIEWER)], WROTE
run(clean(labels=[REVIEWER]), 800)
assert edits() == [], WROTE
# One back, it's reviewed: off.
C.save(V.OUT, {})
status, _ = run(clean(labels=[REVIEWER]), 900)
assert "No reviewer" not in status and edits() == ["pr edit 5 -R sam/r --remove-label waiting: reviewer"], (status, WROTE)

# Held by hand, it waits on what its holder knows, which no waiting label
# names: off, whatever it had. A draft the same.
fresh_pr_state()
status, _ = run(clean(labels=["hold", SLOT]), 600)
assert "On hold" in status and edits() == ["pr edit 5 -R sam/r --remove-label waiting: proof slot"], (status, WROTE)
status, _ = run(clean(labels=[AGE, REVIEWER], draft=True), 600)
assert "Draft" in status and edits() == ["pr edit 5 -R sam/r --remove-label waiting: release age --remove-label waiting: reviewer"], \
    (status, WROTE)
# Labels that aren't the steward's waiting ones are left alone, and a PR
# with none of its waiting ones that waits on none costs no call.
status, _ = run(clean(labels=["first", "bug"]), 700)
assert edits() == [], WROTE
print("waiting labels: ok")
