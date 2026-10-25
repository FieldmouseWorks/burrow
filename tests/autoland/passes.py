"""What starts the steward's passes, as burrow-events runs them: the
wake-ups a pass books, a kicked pass over one repo, each repo's hook, and
the kicks from DeepSeek's reviews and Claude jobs."""

import json
import os
import sys

from harness import (KICKS, REAL, STATES, T, WROTE, C, R, S, clean, codex_clean, fresh_pr_state, head, info, mkpr,
                     queued, run, tmp, worker)

# A pass that waits on time books its repo's next pass for when the wait
# ends: a new head's settle delay, the reviewer's timeout, and how long
# findings sit before a job.
fresh_pr_state()
run(mkpr(), 10)
assert C.WAKE == {"sam/r": T + 10 + 3 * 60 + 1}, C.WAKE
run(mkpr(), 200)  # Codex asked: heard about once its timeout's up
assert C.WAKE == {"sam/r": T + 200 + 45 * 60 + 1}, C.WAKE
fresh_pr_state()
run(clean(threads=[False]), 300)
assert C.WAKE == {"sam/r": T + 300 + 30 * 60 + 1}, C.WAKE
# Nothing else to wait for: a day after it opened, it's listed for the owner.
run(clean(), 400)
assert C.WAKE == {"sam/r": T + 86400 + 1}, C.WAKE
run(clean(), 86400 + 10)
assert C.WAKE == {}, C.WAKE

# Whatever didn't go through is looked at again soon: a merge that failed, a
# job that couldn't start, a job seen ended with no answer (the second look),
# and a DeepSeek review lost from the queue.
fresh_pr_state()
S.land, land = (lambda repo, pr, info: False), S.land
run(clean(), 700)
assert C.WAKE["sam/r"] == T + 700 + 60 + 1, C.WAKE
S.land = land
fresh_pr_state()
C.job, job = (lambda repo, task, clone=False: None), C.job
run(clean(files=["migrations/1.sql"]), 700)
assert C.WAKE["sam/r"] == T + 700 + 5 * 60 + 1, C.WAKE
C.job = job
fresh_pr_state()
run(clean(files=["migrations/1.sql"]), 700)
STATES["job-1"] = "error"
run(clean(files=["migrations/1.sql"]), 800)
assert C.WAKE["sam/r"] == T + 800 + 60 + 1, C.WAKE
STATES.clear()
fresh_pr_state()
run(mkpr(comments=[codex_clean()]), 0)
run(mkpr(comments=[codex_clean()]), 600)
assert queued() == [head]
for x in os.listdir(f"{tmp}/state/ds-queue"):
    os.remove(f"{tmp}/state/ds-queue/{x}")
run(mkpr(comments=[codex_clean()]), 700)
assert C.WAKE["sam/r"] == T + 700 + 60 + 1, C.WAKE
# Three failed reviews: tried again an hour after the last.
st = C.load("pr-sam_r-5.json", None)
st["ds"].update(fails=3, failedAt=T + 650)
st["ds"].pop("gone", None)
st["ds"].pop("queued", None)
C.save("pr-sam_r-5.json", st)
run(mkpr(comments=[codex_clean()]), 800)
assert C.WAKE["sam/r"] == T + 650 + 3600 + 1, C.WAKE
print("wake-ups: ok")

# DeepSeek's review back, posted or failed: the repo's pass is kicked, since a
# failure sends GitHub nothing.
fresh_pr_state()
run(mkpr(comments=[codex_clean()]), 0)
run(mkpr(comments=[codex_clean()]), 600)
KICKS.clear()
worker()
assert KICKS == ["/kick/autoland/sam/r"], KICKS
# A Claude job kicks it at the end of each turn (proj run --kick).
proj = os.path.join(tmp, "proj")
with open(proj, "w") as f:
    f.write(f"#!/bin/sh\necho \"$@\" >{tmp}/proj-args\necho job-9\n")
os.chmod(proj, 0o755)
os.makedirs(f"{tmp}/src/r", exist_ok=True)
kept = dict(S.CFG)
S.CFG.update(proj=proj, projectsDir=f"{tmp}/src")
C.project_dir, project_dir = (lambda repo: "r"), C.project_dir
assert REAL["job"]("sam/r", "the task") == "job-9"
assert open(f"{tmp}/proj-args").read().split() == ["run", "--kick", "autoland/sam/r", "r", "the", "task"]
C.project_dir = project_dir
S.CFG.clear()
S.CFG.update(kept)
print("kicks: ok")

# `kick REPO`: only that repo's PRs, none of the full pass's settings, new
# repos or branch sweep, and its owner's lines and wake-ups replace that
# repo's while the rest stay. The owner's issue is kept up to date from it.
fresh_pr_state()
C.NOW = T
C.save("wake.json", {"sam/r": T + 50, "sam/other": T + 60, "sam/gone": T - 1})
C.save("digest-repos.json", {"sam/r": [["Old", "r line", ""]], "sam/other": [["Open over a day", "other line", "x"]]})
C.save("repos.json", {"untouched": True})
seen_repos, swept, published = [], [], []
kept_fns = (S.R.discover, S.R.settle_repo, S.R.publish, S.R.sweep_branches, S.open_prs)
S.R.discover = lambda: [{"repo": "sam/r"}, {"repo": "sam/other"}]
S.R.settle_repo = lambda r, known: (_ for _ in ()).throw(AssertionError("settings in a kicked pass"))
S.R.publish = published.append
S.R.sweep_branches = lambda r, heads, digest, sweep=True: swept.append(sweep)


def kicked_prs(owner, name):
    seen_repos.append(f"{owner}/{name}")
    return info, [mkpr()]


S.open_prs, argv = kicked_prs, sys.argv
S.CFG["cleanBranches"] = True
sys.argv = ["steward", "kick", "sam/r"]
C.WAKE.clear()
S.main()
sys.argv = argv
S.CFG["cleanBranches"] = False
S.R.discover, S.R.settle_repo, S.R.publish, S.R.sweep_branches, S.open_prs = kept_fns
assert seen_repos == ["sam/r"] and swept == [False], (seen_repos, swept)
assert C.load("repos.json", None) == {"untouched": True}
assert C.load("wake.json", None) == {"sam/r": T + 3 * 60 + 1, "sam/other": T + 60}, C.load("wake.json", None)
assert C.load("digest-repos.json", None) == {"sam/r": [], "sam/other": [["Open over a day", "other line", "x"]]}
assert published == [[("Open over a day", "other line", "x")]], published
print("kick: ok")

# Each repo keeps a hook to burrow-events: added when it's missing, set
# again when its URL, events or secret change, left alone otherwise.
HOOKS, SENT = {"v": []}, []
gh_kept, write_kept = C.gh, C.write
C.gh = lambda *a, **k: HOOKS["v"] if a[:2] == ("api", "repos/sam/r/hooks") else gh_kept(*a, **k)


def write_hook(*a, **k):
    SENT.append(json.loads(k["input"]))
    return write_kept(*a, **k)


C.write = write_hook
fresh_pr_state()
WROTE.clear()
st = {}
R.hook("sam/r", st, False)
add = [x for x in WROTE if "POST repos/sam/r/hooks" in x]
assert len(add) == 1 and st["hook"], WROTE
assert SENT[-1]["config"] == {"url": "https://example.org/burrow/hook", "content_type": "json",
                              "secret": "hook-secret", "insecure_ssl": "0"}, SENT
assert SENT[-1]["events"] == ["check_suite", "push"], SENT
WROTE.clear()
R.hook("sam/r", st, False)
assert WROTE == [], WROTE
HOOKS["v"] = [{"id": 9, "config": {"url": "https://example.org/burrow/hook"}}]
with open(f"{tmp}/cred/webhook", "w") as f:
    f.write("new-secret\n")
R.hook("sam/r", st, False)
assert [x for x in WROTE if "PATCH repos/sam/r/hooks/9" in x] and not [x for x in WROTE if "POST" in x], WROTE
assert SENT[-1]["config"]["secret"] == "new-secret", SENT
C.gh, C.write = gh_kept, write_kept
print("hooks: ok")
