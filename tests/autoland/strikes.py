"""Nothing the steward does loops quietly: a PR that conflicts with its base
goes to a Claude job; every merge state GitHub reports has a step; every
step that fails and is tried again makes the PR's wait overdue on its third
failure in a row, or lists it on the stuck issue without the watchdog; and
every wake-up a pass books says what ends the wait it's on."""

import json
import os
import re

from harness import COMMITS, FAIL, JOBS, STATES, STUB, T, WROTE, C, H, S, clean, fresh_pr_state, merged, mkpr, pr_state, queued, \
    run, said, stand_in, swept, tmp, worker
from touches import DEPENDABOT_EMAIL

import waits

stand_in()
KEY = "sam/r#5"


def dirty(pr):
    return {**pr, "mergeStateStatus": "DIRTY"}


def desc(status):
    return status.partition("description=")[2].partition(" -f target_url")[0]


# A conflicted PR, settled otherwise: a Claude job merges its base in at
# once (GitHub won't merge it and runs no CI on it), and nothing merges it.
fresh_pr_state()
status, _ = run(dirty(clean()), 700)
assert len(JOBS) == 1 and "it conflicts with main" in JOBS[0] and "never rebase or force-push" in JOBS[0], JOBS
assert desc(status).startswith("Conflicts with main; a Claude job has it since") and not merged(), (status, WROTE)
# While it works, nothing more starts; one that ends with the conflict still
# there is handed once more, and after a second the owner hears.
status, _ = run(dirty(clean()), 800)
assert JOBS == [] and "a Claude job has it since" in status, (JOBS, status)
STATES["job-1"] = "done"
status, _ = run(dirty(clean()), 900)
assert JOBS == [] and "its Claude job just ended" in status, (JOBS, status)
run(dirty(clean()), 1000)
assert len(JOBS) == 1 and "it conflicts with main" in JOBS[0], JOBS
run(dirty(clean()), 1100)
status, digest = run(dirty(clean()), 1200)
assert "state=failure" in status and digest[0][0] == "Two Claude jobs ended without fixing it", (status, digest)
STATES.clear()
# Before any review or CI too: neither can settle it while it conflicts.
fresh_pr_state()
status, _ = run(dirty(mkpr(checks=[])), 700)
assert len(JOBS) == 1 and queued() == [] and "Conflicts with main" in status, (JOBS, queued(), status)
# A bot's own update is the bot's to rebase.
fresh_pr_state()
status, _ = run(dirty(clean(branch="renovate/foo-1.x", title="Update foo from 1.4 to 1.5")), 700)
assert JOBS == [] and "Conflicts with main; its bot rebases it" in status and not merged(), (JOBS, status)
# A Dependabot one too, and still open a day on, it's listed for the owner.
dependabot = {"branch": "dependabot/cargo/sqlx-0.8.7", "title": "Bump sqlx from 0.8.6 to 0.8.7", "number": 15}
COMMITS[15] = [DEPENDABOT_EMAIL]
fresh_pr_state()
status, digest = run(dirty(clean(**dependabot)), 90000)
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
    status, _ = run(dirty(pr), 700)
    assert len(JOBS) == 1 and "it conflicts with main" in JOBS[0] and "a Claude job has it" in status, (JOBS, status)
# One renovate-sweep has is its job's: a red one, or a major with no verdict
# on this head, gets no job of ours beside it on the branch.
red = [{"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "FAILURE"}]
for pr in (clean(checks=red, **major), clean(**major)):
    fresh_pr_state()
    status, _ = run(dirty(pr), 700)
    assert JOBS == [] and "Conflicts with main; renovate-sweep has it" in status, (JOBS, status)
    assert waits.NOTED["sam/r"]["sam/r#8"]["kick"] == "renovate-sweep/sam/r", waits.NOTED
# And one it held is the owner's: nothing pushes to it.
fresh_pr_state()
status, _ = run(dirty(clean(comments=[said(swept("held"))], **major)), 700)
assert JOBS == [] and "renovate-sweep held it" in status, (JOBS, status)
COMMITS.clear()
print("conflicts: ok")

# Every merge state GitHub's API has gets a step, and one it doesn't know is
# a refusal: a settled PR is merged, brought up to date, handed to a job for
# its conflicts, or left pending with a strike. Never nothing.
assert set(H.MERGE_STEPS) == {"BEHIND", "BLOCKED", "CLEAN", "DIRTY", "DRAFT", "HAS_HOOKS", "UNKNOWN", "UNSTABLE"}, H.MERGE_STEPS
for state in [*H.MERGE_STEPS, "SOMETHING_NEW"]:
    fresh_pr_state()
    status, _ = run({**clean(), "mergeStateStatus": state}, 700)
    step = H.MERGE_STEPS.get(state, "refused")
    took = {"merge": merged() and "state=success" in status,
            "update": any("/update-branch" in x for x in WROTE) and not merged(),
            "conflict": len(JOBS) == 1 and not merged()}.get(step)
    if took is None:
        took = pr_state()["strikes"] == {"mergeable": 1} and "state=pending" in status and not merged()
    assert took, (state, status, WROTE, JOBS)
print("merge states: ok")

# Every step that's tried again, failing for good: a pass booked a minute
# on, then two, then four, and on the third failure in a row its wait is
# overdue, saying what fails. The watchdog then takes it at once.
real_land, real_job = H.land, C.job


def reviewed():
    run(mkpr(), 0)
    run(mkpr(), 600)
    worker()


CASES = {
    "status": {"pr": clean(labels=["hold"]), "fail": lambda: FAIL.append("/statuses/"), "mend": FAIL.clear},
    "update": {"pr": {**clean(), "mergeStateStatus": "BEHIND"}, "fail": lambda: FAIL.append("/update-branch"),
               "mend": FAIL.clear},
    "merge": {"pr": clean(), "fail": lambda: setattr(H, "land", lambda *a: False),
              "mend": lambda: setattr(H, "land", real_land)},
    "mergeable": {"pr": {**clean(), "mergeStateStatus": "UNKNOWN"}, "mended": clean()},
    "job": {"pr": clean(files=["secrets/1.yaml"]), "fail": lambda: setattr(C, "job", lambda *a, **k: None),
            "mend": lambda: setattr(C, "job", real_job)},
    "post": {"pr": mkpr(), "setup": reviewed, "fail": lambda: STUB.update(post=C.Unavailable("GitHub's down")),
             "mend": lambda: STUB.update(post=None)},
}
assert set(CASES) == set(H.STRIKES), "each step that's tried again has its case here"


def a_pass(pr, at):
    waits.begin()
    waits.read("sam/r", [pr])
    return run(pr, at)


def watched():
    waits.publish({"sam/r"}, {}, False)
    return json.load(open(f"{tmp}/state/watch.json"))["items"][KEY]


for kind, case in CASES.items():
    why = H.STRIKES[kind]
    fresh_pr_state()
    case.get("setup", lambda: None)()
    case.get("fail", lambda: None)()
    for i, at in enumerate((700, 800, 900)):
        _, digest = a_pass(case["pr"], at)
        assert pr_state()["strikes"].get(kind) == i + 1, (kind, pr_state())
        assert C.WAKE["sam/r"] == T + at + 2 ** i * 60 + 1, (kind, i, C.WAKE)
        assert waits.FAILING == ({KEY: why} if i == 2 else {}), (kind, i, waits.FAILING)
        assert not any(x[0] == "Failing again and again" for x in digest), (kind, digest)
    it = watched()
    assert it["due"] == it["since"] == C.NOW and it["phase"].endswith("; " + why), (kind, it)
    # Without the watchdog, it's listed on the stuck issue instead.
    watch, S.CFG["watch"] = S.CFG["watch"], None
    _, digest = a_pass(case["pr"], 1200)
    S.CFG["watch"] = watch
    assert ("Failing again and again", f"[{KEY}](u) t", f"{why}, 4 times in a row (journalctl -u autoland)") in digest, \
        (kind, digest)
    assert C.WAKE["sam/r"] == T + 1200 + 8 * 60 + 1, (kind, C.WAKE)
    # Once it goes through, the run is over, and its wait has its deadline.
    case.get("mend", lambda: None)()
    a_pass(case.get("mended", case["pr"]), 1300)
    assert kind not in pr_state()["strikes"] and waits.FAILING == {}, (kind, pr_state())
    it = watched()
    minutes = S.CFG["defaults"]["holdStallAfter" if kind == "status" else "stallAfter"]  # status's case is on hold
    assert it["due"] == it["since"] + minutes * 60 and why not in it["phase"], (kind, it)
# The backoff stops at an hour, and a new head starts the count over.
fresh_pr_state()
for i in range(9):
    a_pass(CASES["mergeable"]["pr"], 700 + i * 4000)
assert pr_state()["strikes"] == {"mergeable": 9} and C.WAKE["sam/r"] == T + 700 + 8 * 4000 + 3600 + 1, (pr_state(), C.WAKE)
a_pass({**clean(sha="b" * 40), "mergeStateStatus": "UNKNOWN"}, 40000)
assert pr_state()["strikes"] == {"mergeable": 1}, pr_state()
# Two reviews it couldn't post in one pass are that pass's one failure.
fresh_pr_state()
reviewed()
STUB["post"] = C.Unavailable("GitHub's down")
a_pass(mkpr(), 700)
STUB["post"] = None
assert pr_state()["strikes"] == {"post": 1}, pr_state()
print("strikes: ok")

# Every wake-up a pass books says what ends the wait it's on (a strike, a
# deadline the watchdog watches, a listing, a look made once), so a new
# retry can't be added without one.
src = os.path.dirname(H.__file__)
unsaid = [f"{f}:{i}: {x.strip()}" for f in sorted(os.listdir(src)) if f.endswith(".py")
          for i, x in enumerate(open(os.path.join(src, f)), 1)
          if re.search(r"(?<![\w.])after\(|\bC\.wake\(", x) and not x.lstrip().startswith("def ") and "# ends:" not in x]
assert not unsaid, "a wake-up without what ends its wait (# ends: ...):\n" + "\n".join(unsaid)
print("every wait ends: ok")
