"""Nothing the steward does loops quietly: every merge state GitHub reports
has a step (conflicts.py has where a conflict goes); every step that fails
and is tried again makes the PR's wait overdue on its third failure in a
row, or lists it on the stuck issue without the watchdog; and every wake-up
a pass books says what ends the wait it's on."""

import json
import os
import re

from harness import FAIL, JOBS, STUB, T, UNSTOPPABLE, WROTE, C, H, S, clean, fresh_pr_state, merged, mkpr, pr_state, run, \
    stand_in, tmp, worker

import harness
import waits

stand_in()
KEY = "sam/r#5"


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
# overdue, saying what fails, and what GitHub said when it refused (on one
# line). The watchdog then takes it at once.
real_job = C.job
REFUSAL = harness.REFUSAL
SAID = "Repository rule violations found; Merge method squash is not allowed for this base branch (HTTP 405)"


def reworded():
    """GitHub says something else to the next try."""
    harness.REFUSAL = "Base branch was modified. Review and try the merge again. (HTTP 405)"
    return harness.REFUSAL


def mended():
    FAIL.clear()
    harness.REFUSAL = REFUSAL


def reviewed():
    run(mkpr(), 0)
    run(mkpr(), 600)
    worker()


def judging():
    """A judge at work on the PR's head, which it's pushed over next pass."""
    C.job = lambda *a, **k: "judge-on-a"
    run(clean(files=["secrets/1.yaml"]), 600)
    C.job = real_job


CASES = {
    "status": {"pr": clean(labels=["hold"]), "fail": lambda: FAIL.append("/statuses/"), "mend": FAIL.clear},
    "update": {"pr": {**clean(), "mergeStateStatus": "BEHIND"}, "fail": lambda: FAIL.append("/update-branch"),
               "mend": mended, "said": SAID, "again": reworded},
    "merge": {"pr": clean(), "fail": lambda: FAIL.append("/merge"), "mend": mended, "said": SAID, "again": reworded},
    "mergeable": {"pr": {**clean(), "mergeStateStatus": "UNKNOWN"}, "mended": clean()},
    "job": {"pr": clean(files=["secrets/1.yaml"]), "fail": lambda: setattr(C, "job", lambda *a, **k: None),
            "mend": lambda: setattr(C, "job", real_job)},
    "post": {"pr": mkpr(), "setup": reviewed, "fail": lambda: STUB.update(post=C.Unavailable("GitHub's down")),
             "mend": lambda: STUB.update(post=None), "said": "GitHub's down",
             "again": lambda: STUB.update(post=C.Unavailable("GitHub's still down")) or "GitHub's still down"},
    "stop": {"pr": clean(files=["secrets/1.yaml"], sha="b" * 40), "setup": judging,
             "fail": lambda: UNSTOPPABLE.add("judge-on-a"), "mend": UNSTOPPABLE.clear},
}
# A job that broke (lost) is struck once a job, not once a pass: lost.py has its case.
assert set(CASES) | {"lost"} == set(H.STRIKES), "each step that's tried again has its case here, or in lost.py"


def a_pass(pr, at):
    waits.begin()
    waits.read("sam/r", [pr])
    return run(pr, at)


def watched():
    waits.publish({"sam/r"}, {}, False)
    return json.load(open(f"{tmp}/state/watch.json"))["items"][KEY]


for kind, case in CASES.items():
    why = H.STRIKES[kind]
    shown = why + (f": {case['said']}" if "said" in case else "")
    fresh_pr_state()
    case.get("setup", lambda: None)()
    case.get("fail", lambda: None)()
    for i, at in enumerate((700, 800, 900)):
        _, digest = a_pass(case["pr"], at)
        assert pr_state()["strikes"].get(kind) == i + 1, (kind, pr_state())
        assert C.WAKE["sam/r"] == T + at + 2 ** i * 60 + 1, (kind, i, C.WAKE)
        assert waits.FAILING == ({KEY: why} if i == 2 else {}), (kind, i, waits.FAILING)
        assert waits.SAID == ({KEY: case["said"]} if i == 2 and "said" in case else {}), (kind, i, waits.SAID)
        assert not any(x[0] == "Failing again and again" for x in digest), (kind, digest)
    it = watched()
    assert it["due"] == it["since"] == C.NOW and it["phase"].endswith("; " + shown), (kind, it)
    # Without the watchdog, it's listed on the stuck issue instead.
    watch, S.CFG["watch"] = S.CFG["watch"], None
    _, digest = a_pass(case["pr"], 1200)
    S.CFG["watch"] = watch
    listed = f"{H.STRIKES[kind]}, 4 times in a row" + (f": {case['said']}" if "said" in case else " (journalctl -u autoland)")
    assert ("Failing again and again", f"[{KEY}](u) t", listed) in digest, (kind, digest)
    assert C.WAKE["sam/r"] == T + 1200 + 8 * 60 + 1, (kind, C.WAKE)
    # What GitHub says can change while the failure goes on: the wait shows
    # the latest, and its stall (since when) goes on, so the watchdog's
    # steps on it aren't lost.
    if "again" in case:
        said = case["again"]()
        a_pass(case["pr"], 1250)
        later = watched()
        assert later["since"] == it["since"] and later["phase"].endswith(f"; {why}: {said}"), (kind, later, it)
    # Once it goes through, the run is over, and its wait has its deadline.
    case.get("mend", lambda: None)()
    a_pass(case.get("mended", case["pr"]), 1300)
    assert kind not in pr_state()["strikes"] and waits.FAILING == {}, (kind, pr_state())
    it = watched()
    minutes = S.CFG["defaults"]["holdStallAfter" if kind == "status" else "stallAfter"]  # status's case is on hold
    assert it["due"] == it["since"] + minutes * 60 and H.STRIKES[kind] not in it["phase"], (kind, it)
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
