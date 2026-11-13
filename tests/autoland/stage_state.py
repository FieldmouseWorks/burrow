"""A PR's saved stage, which proj's snapshot lists (prs.stage_item): kept
while it stays in its stage, started when there's no record of one (a draft
or held PR's null stage included, for fresh and legacy state alike), reset by
a transition, gone once the PR isn't open and fresh when it's reopened; and
the snapshot's subscribers are told of each change once the pass is over."""

import json
import os
import stat

import prs
from harness import C, WROTE, clean, fresh_pr_state, run, tmp

KEY = "pr-sam_r-5.json"


def saved():
    return C.load(KEY, {}).get("stage")


# A draft, never saved before: a null stage starts its clock, and the pass
# still gets to its end (merge cancellation, the watchdog's bookkeeping).
fresh_pr_state()
C.STAGE_NEWS.clear()
status, _ = run(clean(draft=True, labels=["waiting: reviewer"]), 600)
assert "Draft" in status, (status, WROTE)
first = saved()
assert first["stage"] is None and first["since"] == C.NOW and first["repo"] == "sam/r" and first["number"] == 5, first
assert C.STAGE_NEWS == [("sam/r", 5)], C.STAGE_NEWS

# Legacy state, with a stage record without a `since` or none at all, starts one.
for legacy in ({}, {"stage": None}, {"stage": {"stage": None}}, {"stage": {"stage": "ci"}}):
    assert prs.stage_item(legacy.get("stage"), "sam/r", 5, None, None, "d")["since"] == C.NOW, legacy
    assert prs.stage_item(legacy.get("stage"), "sam/r", 5, "ci", None, "d")["since"] == C.NOW, legacy

# Again in the same stage, later: its clock is kept, and a pass that changed
# nothing has no news. A transition resets it.
C.STAGE_NEWS.clear()
run(clean(draft=True, labels=["waiting: reviewer"]), 700)
assert saved()["since"] == first["since"] and C.STAGE_NEWS == [], (saved(), C.STAGE_NEWS)
run(clean(labels=["hold", "waiting: reviewer"]), 800)
assert saved()["stage"] is None and saved()["since"] == first["since"]
run(clean(), 900)
assert saved()["stage"] is not None and saved()["since"] == C.NOW, saved()
then = saved()["since"]
run(clean(), 1000)
assert saved()["since"] == then, saved()

# Not open any more: the record goes (the file's other facts stay), and a
# reopening starts its stage anew. Other repos' and PRs' records stay.
other = C.load("pr-sam_r-6.json", {})
other["stage"] = {"repo": "sam/r", "number": 6, "stage": "ci", "by": None, "on": "d", "since": 1}
C.save("pr-sam_r-6.json", other)
C.save("pr-sam_q-5.json", {"stage": {"repo": "sam/q", "number": 5, "stage": "ci", "by": None, "on": "d", "since": 1}})
C.STAGE_NEWS.clear()
prs.retire_stages("sam/r", {6})
assert "stage" not in C.load(KEY, {}) and C.load(KEY, {}).get("head"), C.load(KEY, {})
assert C.load("pr-sam_r-6.json", {}).get("stage") and C.load("pr-sam_q-5.json", {}).get("stage")
assert C.STAGE_NEWS == [("sam/r", 5)], C.STAGE_NEWS
run(clean(), 2000)
assert saved()["since"] == C.NOW, saved()

# A repo renamed, transferred or respelled: its old name's records go, under
# whatever file they sit in, while the inventory's own names' stay.
C.save("pr-old_r-1.json", {"head": "x", "stage": {"repo": "old/r", "number": 1, "stage": "ci", "by": None, "on": "d", "since": 1}})
C.save("pr-Sam_r-5.json", {"stage": {"repo": "Sam/r", "number": 5, "stage": "ci", "by": None, "on": "d", "since": 1}})
C.STAGE_NEWS.clear()
prs.retire_untracked_stages({"sam/r", "sam/q"})
assert "stage" not in C.load("pr-old_r-1.json", {}) and C.load("pr-old_r-1.json", {}).get("head") == "x"
assert "stage" not in C.load("pr-Sam_r-5.json", {})
assert saved() and C.load("pr-sam_q-5.json", {}).get("stage") and len(C.STAGE_NEWS) == 2, C.STAGE_NEWS

# A repo's reset deletes its PRs' files: the removal is news, though no PR is left.
import repos as R
C.save("repo-sam_q.json", {})
R.retire_checkouts = lambda repo, k: None
R.V.sweep = lambda k, keep: []
C.STAGE_NEWS.clear()
R.reset({"repo": "sam/q"}, "sam_q", set(), {})
assert not os.path.exists(f"{C.STATE}/pr-sam_q-5.json") and C.STAGE_NEWS == [("sam/q", 5)], C.STAGE_NEWS

# The subscribers are told once, after the pass, and only when something changed.
told = f"{tmp}/told"
fake = f"{tmp}/proj-fake"
with open(fake, "w") as f:
    f.write(f'#!/bin/sh\necho "$@" >> {told}\n')
os.chmod(fake, os.stat(fake).st_mode | stat.S_IXUSR)
real, C.CFG["proj"] = C.CFG["proj"], fake
try:
    C.STAGE_NEWS.clear()
    prs.stage_news()
    assert not os.path.exists(told) and C.STAGE_NEWS == [], "nothing changed: nobody told"
    C.STAGE_NEWS.extend([("sam/r", 5), ("sam/r", 6)])
    prs.stage_news()
    assert open(told).read() == "_notify\n" and C.STAGE_NEWS == []
finally:
    C.CFG["proj"] = real
