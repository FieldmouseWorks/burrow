"""What starts the steward's passes, as burrow-events runs them: the
wake-ups a pass books, a kicked pass over one repo, each repo's hook, and
the kicks from DeepSeek's reviews and Claude jobs."""

import json
import os
import sys

from harness import (FAIL, KICKS, QDIR, REAL, STATES, STUB, T, WROTE, C, R, S, V, W, clean, fresh_pr_state, head,
                     info, mkpr, pr_state, queued, run, rv_review, tmp, worker)

# A pass that waits on time books its repo's next pass for when the wait
# ends: a new head's settle delay, and how long findings sit before a job.
fresh_pr_state()
run(mkpr(), 10)
assert C.WAKE == {"sam/r": T + 10 + 3 * 60 + 1}, C.WAKE
# The reviewers queued: their answers kick it (below), so there's only the day.
run(mkpr(), 200)
assert queued() == ["codex", "deepseek"] and C.WAKE == {"sam/r": T + 86400 + 1}, C.WAKE
fresh_pr_state()
run(clean(threads=[False]), 300)
assert C.WAKE == {"sam/r": T + 300 + 30 * 60 + 1}, C.WAKE
# Nothing else to wait for: a day after it opened, it's listed for the owner.
run(clean(), 400)
assert C.WAKE == {"sam/r": T + 86400 + 1}, C.WAKE
run(clean(), 86400 + 10)
assert C.WAKE == {}, C.WAKE
# A status or a stop of auto-merge GitHub didn't take is tried again a
# minute later: until then the old success and the auto-merge stand.
fresh_pr_state()
run(clean(), 400)
key = "pr-sam_r-5.json"
C.save(key, {**C.load(key, {}), "landed": head})
held = clean(labels=["hold"])
held["autoMergeRequest"] = {"enabledAt": "x"}
FAIL.extend(["/statuses/", "--disable-auto"])
run(held, 500)
assert C.WAKE["sam/r"] == T + 500 + 60 + 1 and C.load(key, {})["landed"] == head, (C.WAKE, C.load(key, {}))
FAIL.remove("/statuses/")
run(held, 600)
assert C.WAKE["sam/r"] == T + 600 + 60 + 1, C.WAKE
FAIL.remove("--disable-auto")
run(held, 700)
assert C.WAKE == {"sam/r": T + 86400 + 1} and C.load(key, {})["landed"] is None, (C.WAKE, C.load(key, {}))
FAIL.append("/statuses/")
run(clean(labels=["hold"]), 800)
assert C.WAKE["sam/r"] == T + 800 + 60 + 1, C.WAKE
FAIL.clear()
# A red-branch fix the branch went green without is held the same way while
# it waits to be closed: its status goes pending and its queued merge comes
# off, again a minute later if GitHub doesn't take that.
fresh_pr_state()
run(clean(), 400)
C.save(key, {**C.load(key, {}), "landed": head})
stale = {**clean(branch="autoland/red-cccccccccccc"), "autoMergeRequest": {"enabledAt": "x"}}
FAIL.append("--disable-auto")
status, _ = run(stale, 500, obsolete=True)
assert "state=pending" in status and "green again without it" in status, status
assert C.WAKE["sam/r"] == T + 500 + 60 + 1 and C.load(key, {})["landed"] == head, (C.WAKE, C.load(key, {}))
FAIL.clear()
run(stale, 600, obsolete=True)
assert "pr merge 5 -R sam/r --disable-auto" in WROTE and C.load(key, {})["landed"] is None, (WROTE, C.load(key, {}))
# Every finding answered: the reviewer looks again once the settle delay
# after its review is up.
fresh_pr_state()
run(mkpr(reviews=[rv_review("codex", 1), rv_review("deepseek", 0)]), 700)
assert C.WAKE == {"sam/r": T + 660 + 3 * 60 + 1}, C.WAKE
# A reviewer out of service: its look is asked for once it's back.
fresh_pr_state()
run(mkpr(), 0)
C.save(V.OUT, {"codex": {"why": "its usage is spent", "until": T + 5000}})
run(mkpr(), 600)
assert queued() == ["deepseek"] and C.WAKE == {"sam/r": T + 5000 + 1}, C.WAKE
C.save(V.OUT, {})

# Whatever didn't go through is looked at again soon: a merge that failed, a
# job that couldn't start, a job seen ended with no answer (the second look),
# a review that couldn't be queued or posted, and one lost from the queue.
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
run(mkpr(), 0)
STUB["queue"] = RuntimeError("git: the head moved on")
run(mkpr(), 600)
STUB["queue"] = None
assert queued() == [] and C.WAKE["sam/r"] == T + 600 + 5 * 60 + 1, C.WAKE
fresh_pr_state()
run(mkpr(), 0)
run(mkpr(), 600)
worker()
STUB["post"] = RuntimeError("gh: 502")
run(mkpr(), 700)
STUB["post"] = None
assert C.WAKE["sam/r"] == T + 700 + 60 + 1, C.WAKE
fresh_pr_state()
run(mkpr(), 0)
run(mkpr(), 600)
assert queued() == ["codex", "deepseek"]
for x in os.listdir(QDIR):
    os.remove(f"{QDIR}/{x}")
run(mkpr(), 700)
assert C.WAKE["sam/r"] == T + 700 + 60 + 1, C.WAKE
# Three failed reviews: tried again an hour after the last.
st = pr_state()
for r in ("codex", "deepseek"):
    st["rv"][r] = {"fails": 3, "failedAt": T + 650}
C.save("pr-sam_r-5.json", st)
run(mkpr(), 800)
assert C.WAKE["sam/r"] == T + 650 + 3600 + 1, C.WAKE
print("wake-ups: ok")

# A reviewer's answer back, a review or why there's none: the repo's pass is
# kicked, since nothing goes to GitHub until that pass posts it.
fresh_pr_state()
C.save(f"{V.QUEUE}/codex--lost.json", {"reviewer": "codex", "repo": "sam/r", "number": 5, "snap": "gone",
                                       "headRefOid": head})
KICKS.clear()
W.review("codex--lost.json")
assert C.load(f"{V.DONE}/codex--lost.json", None)["result"] == "error" and queued() == []
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
assert open(f"{tmp}/proj-args").read().split() == ["run", "--timeout", "60m", "--idle", "20m", "--kick", "autoland/sam/r", "r", "the", "task"]
# One of the pipeline's own repos goes ahead of the line.
assert REAL["job"]("sam/pipe", "the task") == "job-9"
assert open(f"{tmp}/proj-args").read().split() == ["run", "--first", "--timeout", "60m", "--idle", "20m", "--kick", "autoland/sam/pipe", "r", "the", "task"]
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
kept_fns = (S.R.discover, S.R.settle_repo, S.R.publish, S.R.sweep_branches, S.open_prs, S.R.unhook_gone)
S.R.unhook_gone = lambda repos: (_ for _ in ()).throw(AssertionError("hooks taken off in a kicked pass"))
S.R.discover = lambda: [{"repo": "sam/r"}, {"repo": "sam/other"}]
S.R.settle_repo = lambda r, known: (_ for _ in ()).throw(AssertionError("settings in a kicked pass"))
S.R.publish = published.append
in_use = []
S.R.sweep_branches = lambda r, branches, digest, sweep=True: swept.append(sweep) or in_use.append(branches)


def kicked_prs(owner, name):
    seen_repos.append(f"{owner}/{name}")
    return info, [mkpr()]


S.open_prs, argv = kicked_prs, sys.argv
S.CFG["cleanBranches"] = True
sys.argv = ["steward", "kick", "Sam/R"]  # as burrow-events spells it, or not
C.WAKE.clear()
S.main()
sys.argv = argv
S.CFG["cleanBranches"] = False
S.R.discover, S.R.settle_repo, S.R.publish, S.R.sweep_branches, S.open_prs, S.R.unhook_gone = kept_fns
assert seen_repos == ["sam/r"] and swept == [False], (seen_repos, swept)
# The sweep leaves every open PR's branches, its base as well as its head:
# deleting a base closes the PRs on it.
assert in_use == [{"feat", "main"}], in_use
assert C.load("repos.json", None) == {"untouched": True}
assert C.load("wake.json", None) == {"sam/r": T + 3 * 60 + 1, "sam/other": T + 60}, C.load("wake.json", None)
assert C.load("digest-repos.json", None) == {"sam/r": [], "sam/other": [["Open over a day", "other line", "x"]]}
assert published == [[("Open over a day", "other line", "x")]], published
print("kick: ok")

# A long PR's older reviews and comments are read too, a page at a time,
# oldest first: its verdicts, markers and rounds can be past the last 100.
def page(nodes, more=False, at=None):
    return {"pageInfo": {"hasPreviousPage": more, "startCursor": at}, "nodes": nodes}


ASKED = []


def graphql(query, **v):
    if "node(id" in query and "comments(" in query:
        ASKED.append(("older comments", v["id"]))
        return {"node": {"comments": page(["c"], len(ASKED) < 30, "c-1")}}
    ASKED.append(("older reviews" if "node(id" in query else "prs", v.get("cursor")))
    if "node(id" in query:
        return {"node": {"reviews": page(["r1"])}}
    who = {"isDraft": False, "isCrossRepository": False, "author": {"login": "sam"}}
    pr = {"id": "PR_1", **who, "reviews": page(["r2"], True, "c-r2"), "comments": page(["c1"])}
    # One whose comments go on and on (anyone can add them) is read only so
    # far: past that the cut hands it to the owner.
    endless = {"id": "PR_3", **who, "reviews": page([]), "comments": page(["c"], True, "c-0")}
    # An outside contribution's older pages aren't read: anyone can add to them.
    outside = {"id": "PR_2", **who, "author": {"login": "eve"}, "reviews": page(["x"], True, "c-x"), "comments": page([])}
    return {"repository": {"squashMergeAllowed": True, "pullRequests": {"pageInfo": {"hasNextPage": False}, "nodes": [pr, outside, endless]}}}


graphql_kept, C.graphql = C.graphql, graphql
_, prs = S.open_prs("sam", "r")
C.graphql = graphql_kept
assert prs[0]["reviews"] == page(["r1", "r2"]) and prs[0]["comments"] == page(["c1"]), prs
assert ASKED == [("prs", None), ("older reviews", "c-r2")] + [("older comments", "PR_3")] * S.OLDER_PAGES, ASKED
assert len(prs[2]["comments"]["nodes"]) == 1 + S.OLDER_PAGES and prs[2]["comments"]["pageInfo"]["hasPreviousPage"], prs[2]
# Cut short, it's still just an outside contribution.
fresh_pr_state()
long_outside = mkpr()
long_outside["author"] = {"login": "eve"}
long_outside["comments"]["pageInfo"]["hasPreviousPage"] = True
statuses, digest = run(long_outside, 600)
assert "Outside contribution" in statuses and [x[0] for x in digest] == ["Outside contributions"], (statuses, digest)
print("long PRs: ok")

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
# A new URL moves the hook it made, by its id, rather than adding another.
assert st["hookId"] == 9, st
WROTE.clear()
S.CFG["webhook"] = {**S.CFG["webhook"], "url": "https://new.example.org/burrow/hook"}
R.hook("sam/r", st, False)
assert [x for x in WROTE if "PATCH repos/sam/r/hooks/9" in x] and not [x for x in WROTE if "POST" in x], WROTE
assert SENT[-1]["config"]["url"] == "https://new.example.org/burrow/hook", SENT
S.CFG["webhook"] = {**S.CFG["webhook"], "url": "https://example.org/burrow/hook"}
# A status run, from a shell without the services' credentials, leaves the
# hook alone instead of failing on the secret.
cred = os.environ["CREDENTIALS_DIRECTORY"]
os.environ["CREDENTIALS_DIRECTORY"] = f"{tmp}/none"
R.DRY = True
WROTE.clear()
R.hook("sam/r", {}, True)
assert WROTE == [], WROTE
R.DRY = False
os.environ["CREDENTIALS_DIRECTORY"] = cred
C.gh, C.write = gh_kept, write_kept
# A project of proj's that it doesn't see to (excluded, another owner's)
# gets the hook too, for proj-watch and proj-inbox, and keeps it.
projects_kept = C.projects
C.projects = lambda: iter([("r", "sam/r"), ("R", "Sam/R"), ("p", "eve/p")])
C.gh = lambda *a, **k: [] if a[:2] == ("api", "repos/eve/p/hooks") else gh_kept(*a, **k)
C.NOW = T
WROTE.clear()
more = R.hook_projects([{"repo": "sam/r"}])
assert more == [{"repo": "eve/p"}], more
assert [x for x in WROTE if "hooks" in x] == ["api -X POST repos/eve/p/hooks --input -"], WROTE
assert C.load("repo-eve_p.json", None)["hookAt"] == T
C.save("repo-eve_p.json", {"hook": "h", "hookId": 5, "hookAt": T})
WROTE.clear()
R.unhook_gone([{"repo": "sam/r"}] + more)
assert not [x for x in WROTE if "eve/p" in x], WROTE
os.remove(f"{tmp}/state/repo-eve_p.json")
C.gh, C.projects = gh_kept, projects_kept
# A repo no longer seen to (excluded, archived) has its hook taken off, on
# a full pass; one that won't come off is tried again a day later.
C.save("repo-sam_old.json", {"hook": "h", "hookId": 7, "other": 1})
C.save("repo-sam_stuck.json", {"hook": "h", "hookId": 8})
C.save("repo-sam_r.json", {"hook": "h", "hookId": 9})
# sam/R and sam/was renamed to sam/r: the same repo, whose hook hook() just
# adopted under the new name, and GitHub answers either old name with it.
C.save("repo-sam_R.json", {"hook": "h", "hookId": 9})
C.save("repo-sam_was.json", {"hook": "h", "hookId": 9, "other": 2})
# sam/once was renamed to sam/r too, but GitHub doesn't answer the lookup:
# the old path may lead to sam/r, so nothing is deleted until it does.
C.save("repo-sam_once.json", {"hook": "h", "hookId": 9})
NAMES = {"repos/sam/R": "sam/r", "repos/sam/was": "sam/r", "repos/sam/old": "sam/old", "repos/sam/stuck": "sam/stuck"}
C.gh = lambda *a, **k: {"full_name": NAMES[a[1]]} if a[:1] == ("api",) and a[1:2] and a[1] in NAMES else gh_kept(*a, **k)
FAIL.append("sam/stuck/hooks")
C.NOW = T
WROTE.clear()
R.unhook_gone([{"repo": "sam/r"}])
assert sorted(x for x in WROTE if "DELETE" in x) == ["api -X DELETE repos/sam/old/hooks/7",
                                                     "api -X DELETE repos/sam/stuck/hooks/8"], WROTE
assert C.load("repo-sam_old.json", None) == {"other": 1}
assert C.load("repo-sam_stuck.json", None) == {"hook": "h", "hookId": 8, "unhookTried": T}
assert C.load("repo-sam_r.json", None)["hookId"] == 9
assert C.load("repo-sam_R.json", None) == {}
assert C.load("repo-sam_was.json", None) == {"other": 2}
assert C.load("repo-sam_once.json", None) == {"hook": "h", "hookId": 9, "unhookTried": T}
WROTE.clear()
R.unhook_gone([{"repo": "sam/r"}])
assert WROTE == [], WROTE
C.NOW = T + 86400 + 1
R.unhook_gone([{"repo": "sam/r"}])
assert WROTE == ["api -X DELETE repos/sam/stuck/hooks/8"], WROTE
NAMES["repos/sam/once"] = "sam/r"
C.NOW = T + 2 * (86400 + 1)
WROTE.clear()
R.unhook_gone([{"repo": "sam/r"}])
assert C.load("repo-sam_once.json", None) == {}
assert "DELETE repos/sam/once" not in " ".join(WROTE), WROTE
FAIL.clear()
C.gh = gh_kept
print("hooks: ok")
