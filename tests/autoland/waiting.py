"""A PR's stage label (prs.show_waiting): every status the steward posts names
the stage of the pipeline the PR is at (stages.py), and the step that posts
it puts that stage's label on and the other stages' off, in one call. A PR
whose labels are already right costs no call, one GitHub didn't take is
asked for again by the next pass, and a draft or a held PR carries none.
No status can go up without a stage: settle takes none without one, and
every call to it in the steward names one."""

import ast
import glob
import itertools
import json
import os

import proofs as P
import stages
from harness import CI_GREEN, FAIL, JOBS, WAITING, WROTE, C, S, T0, V, clean, fresh_pr_state, mkpr, run, rv_review, said

AGE, SLOT, REVIEWER = "waiting: release age", "waiting: proof slot", "waiting: reviewer"
REVIEW, FIX, MACHINE, MERGE = "waiting: review", "waiting: fix", "waiting: machine checks", "waiting: merge"


def edits():
    """The label edits this pass wrote."""
    return [x for x in WROTE if x.startswith("pr edit ")]


def on(n, *labels, off=()):
    return f"pr edit {n} -R sam/r " + " ".join([*(f"--add-label {x}" for x in labels), *(f"--remove-label {x}" for x in off)])


# Every stage the steward names has a label, and every label a stage: the
# module's own (the check passes labels.nix's keys in), else the harness's.
keys = json.loads(os.environ.get("AUTOLAND_STAGES") or json.dumps(list(WAITING)))
assert sorted(stages.ALL) == sorted(keys) == sorted(WAITING), (stages.ALL, keys)

# Every call to settle names its stage, by keyword, so a status can't go up
# without one even on a path no test here takes (settle has no default for it).
here = os.path.dirname(os.path.abspath(S.__file__))


def unstaged(path):
    """The lines of the calls to settle in path that don't name a stage."""
    calls = [x for x in ast.walk(ast.parse(open(path).read()))
             if isinstance(x, ast.Call) and getattr(x.func, "id", None) == "settle"]
    return [x.lineno for x in calls if not any(k.arg == "stage" for k in x.keywords)]


missing = {os.path.basename(p): lines for p in glob.glob(f"{here}/*.py") if (lines := unstaged(p))}
assert missing == {}, f"settle without a stage: {missing}"
# The check catches one that doesn't.
bare = f"{os.environ['HOME']}/bare.py"
with open(bare, "w") as f:
    f.write('settle("pending", "Settled; waiting for CI", stage=S.CI)\nsettle("pending", "Settled; next pass")\n')
assert unstaged(bare) == [2], unstaged(bare)

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
# Refreshed with releases old enough, it's green and merging: the label moves
# with it in one call.
status, _ = run(mkpr(labels=[AGE], **lock), 7400)
assert "nothing in it old enough" not in status and edits() == [on(16, MERGE, off=[AGE])], (status, WROTE)
# Red for any other reason, renovate-sweep fixes it.
status, _ = run(mkpr(checks=[*CI_GREEN, artifacts], comments=[said("Not now.")], **lock), 7500)
assert "CI red; renovate-sweep has it" in status and edits() == [on(16, FIX)], (status, WROTE)

# Running the machine's own checks; waiting its turn for a proof slot, and
# not once the turn has come.
kept, repos = C.job, dict(S.CFG["repos"])
ids = itertools.count(1)
C.job = lambda repo, task, clone=False, since=None, role=None: JOBS.append(task) or f"job-{next(ids)}"
S.CFG["repos"]["sam/r"] = {"proofJobs": {"remi-proof": "Run `just proof` and post the remi-proof status."}, "proofSlots": 1}
proof = CI_GREEN + [{"__typename": "StatusContext", "context": "remi-proof", "state": "PENDING", "description": ""}]
fresh_pr_state()
P.begin("sam/r", {1, 2})
status, _ = run(clean(checks=proof, number=1), 700)
assert "a Claude job is running remi-proof" in status and edits() == [on(1, MACHINE)], (status, WROTE)
status, _ = run(clean(checks=proof, number=2), 700)
assert "waiting its turn to run remi-proof (1 ahead)" in status and edits() == [on(2, SLOT)], (status, WROTE)
# Moved from one wait to another, the label goes with it in one call: put
# back for a reviewer out of service with its proof slot's still on.
C.save(V.OUT, {r: {"why": "spent", "until": C.ts(T0) + 86400} for r in V.NAMES})
status, _ = run(mkpr(checks=proof, number=2, labels=[SLOT]), 800)
assert "No reviewer can review it now" in status, status
assert edits() == [on(2, REVIEWER, off=[SLOT])], WROTE
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
# One back, it's reviewed.
C.save(V.OUT, {})
fresh_pr_state()
status, _ = run(mkpr(labels=[REVIEWER]), 900)
assert "No reviewer" not in status and edits() == [on(5, REVIEW, off=[REVIEWER])], (status, WROTE)
# A finding comes back: a Claude job has it.
status, _ = run(mkpr(reviews=[rv_review("codex", 1)], threads=[False], labels=[REVIEW]), 7200)
assert "1 open review thread" in status and edits() == [on(5, FIX, off=[REVIEW])], (status, WROTE)

# Held by hand, it waits on what its holder knows, and the hold label says
# so: no stage's label, whatever it had. A draft the same.
fresh_pr_state()
status, _ = run(clean(labels=["hold", SLOT]), 600)
assert "On hold" in status and edits() == ["pr edit 5 -R sam/r --remove-label waiting: proof slot"], (status, WROTE)
status, _ = run(clean(labels=[AGE, REVIEWER], draft=True), 600)
assert "Draft" in status and edits() == ["pr edit 5 -R sam/r --remove-label waiting: reviewer --remove-label waiting: release age"], \
    (status, WROTE)
# Whatever path posts its status, a held PR's isn't a stage's: one with more
# than a page to read is the owner's, but on hold it carries no label.
def long(**kw):
    pr = mkpr(**kw)
    pr["comments"]["pageInfo"]["hasPreviousPage"] = True
    return pr


status, _ = run(long(labels=["hold", REVIEW]), 650)
assert "Too many comments" in status and edits() == [f"pr edit 5 -R sam/r --remove-label {REVIEW}"], (status, WROTE)
status, _ = run(long(), 660)
assert "Too many comments" in status and edits() == [on(5, "waiting: Sam")], (status, WROTE)
# Labels that aren't the steward's stage labels are left alone.
status, _ = run(clean(labels=["first", "bug"]), 700)
assert edits() == [on(5, MERGE)], WROTE
print("stage labels: ok")
