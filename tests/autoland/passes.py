"""What starts the steward's passes, as burrow-events runs them: the
wake-ups a pass books, a kicked pass over one repo, and the kicks from
reviews and Claude jobs coming back."""

import json
import os
import sys
import time

from harness import (FAIL, REAL_GH, QDIR, REAL, STATES, STUB, T, UPKEEP_RAN, WROTE, C, P, H, S, V, clean, fresh_pr_state,
                     head, info, mkpr, pr_state, queued, run, rv_review, tmp, worker)

# A pass that waits on time books its repo's next pass for when the wait
# ends: a new head's settle delay, and how long findings sit before a job.
fresh_pr_state()
run(mkpr(), 10)
assert C.WAKE == {"sam/r": T + 10 + 3 * 60 + 1}, C.WAKE
# The reviewers queued: their answers kick it (below), so there's only the day.
run(mkpr(), 200)
assert queued() == ["codex", "rival"] and C.WAKE == {"sam/r": T + 86400 + 1}, C.WAKE
fresh_pr_state()
run(clean(threads=[False]), 300)
assert C.WAKE == {"sam/r": T + 300 + 30 * 60 + 1}, C.WAKE
# Nothing else to wait for: a day after it opened, it's listed for the owner.
run(clean(), 400)
assert C.WAKE == {"sam/r": T + 86400 + 1}, C.WAKE
run(clean(), 86400 + 10)
assert C.WAKE == {}, C.WAKE
# A status or a stop of auto-merge GitHub didn't take is tried again a
# minute later, two after a second failure in a row: until then the old
# success and the auto-merge stand.
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
assert C.WAKE["sam/r"] == T + 600 + 120 + 1, C.WAKE
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
status, _ = run(stale, 500, held="Not needed: main is green again without it")
assert "state=pending" in status and "green again without it" in status, status
assert C.WAKE["sam/r"] == T + 500 + 60 + 1 and C.load(key, {})["landed"] == head, (C.WAKE, C.load(key, {}))
FAIL.clear()
run(stale, 600, held="Not needed: main is green again without it")
assert "pr merge 5 -R sam/r --disable-auto" in WROTE and C.load(key, {})["landed"] is None, (WROTE, C.load(key, {}))
# Every finding answered: the reviewer looks again once the settle delay
# after its review is up.
fresh_pr_state()
run(mkpr(reviews=[rv_review("codex", 1), rv_review("rival", 0)]), 700)
assert C.WAKE == {"sam/r": T + 660 + 3 * 60 + 1}, C.WAKE
# A reviewer out of service: its look is asked for once it's back.
fresh_pr_state()
run(mkpr(), 0)
C.save(V.OUT, {"codex": {"why": "its usage is spent", "until": T + 5000}})
run(mkpr(), 600)
assert queued() == ["rival"] and C.WAKE == {"sam/r": T + 5000 + 1}, C.WAKE
C.save(V.OUT, {})

# Whatever didn't go through is looked at again soon: a merge that failed, a
# job that couldn't start, a job seen ended with no answer (the second look),
# a review that couldn't be queued or posted, and one lost from the queue.
fresh_pr_state()
H.land, land = (lambda repo, pr, info: "refused"), H.land
run(clean(), 700)
assert C.WAKE["sam/r"] == T + 700 + 60 + 1, C.WAKE
H.land = land
fresh_pr_state()
C.job, job = (lambda repo, task, clone=False, since=None: None), C.job
run(clean(files=["secrets/1.yaml"]), 700)
assert C.WAKE["sam/r"] == T + 700 + 60 + 1, C.WAKE
C.job = job
fresh_pr_state()
run(clean(files=["secrets/1.yaml"]), 700)
STATES["job-1"] = "error"
run(clean(files=["secrets/1.yaml"]), 800)
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
assert queued() == ["codex", "rival"]
for x in os.listdir(QDIR):
    os.remove(f"{QDIR}/{x}")
run(mkpr(), 700)
assert C.WAKE["sam/r"] == T + 700 + 60 + 1, C.WAKE
# Three failed reviews: tried again an hour after the last.
st = pr_state()
for r in ("codex", "rival"):
    st["rv"][r] = {"fails": 3, "failedAt": T + 650}
C.save("pr-sam_r-5.json", st)
run(mkpr(), 800)
assert C.WAKE["sam/r"] == T + 650 + 3600 + 1, C.WAKE
print("wake-ups: ok")

# A reviewer's answer back kicks the repo's pass (burrow-review's tests),
# and so does a Claude job, at the end of each turn (proj run --kick).
fresh_pr_state()
proj = os.path.join(tmp, "proj")
with open(proj, "w") as f:
    f.write(f"#!/bin/sh\necho \"$@\" >{tmp}/proj-args\necho job-9\n")
os.chmod(proj, 0o755)
os.makedirs(f"{tmp}/src/r", exist_ok=True)
kept = dict(S.CFG)
S.CFG.update(proj=proj, projectsDir=f"{tmp}/src")
C.project_dir, project_dir = (lambda repo: "r"), C.project_dir
assert REAL["job"]("sam/r", "the task") == "job-9"
assert open(f"{tmp}/proj-args").read().split() == ["run", "--pipeline", "--timeout", "60m", "--idle", "20m", "--kick", "autoland/sam/r", "--owned", "r", "the", "task"]
# One that's project work, with a timeout of its own (a flaky test's).
assert REAL["job"]("sam/r", "the task", pipeline=False, timeout=240) == "job-9"
assert open(f"{tmp}/proj-args").read().split() == ["run", "--timeout", "240m", "--idle", "20m", "--kick", "autoland/sam/r", "--owned", "r", "the", "task"]
# With a machinery App, the job is the machine's own work, and acts as it.
S.CFG["machinery"] = "workers"
assert REAL["job"]("sam/r", "the task") == "job-9"
assert open(f"{tmp}/proj-args").read().split() == ["run", "--pipeline", "--app", "workers", "--timeout", "60m", "--idle", "20m", "--kick", "autoland/sam/r", "--owned", "r", "the", "task"]
# A PR's job stands in line from when the PR was opened (--since, whole
# seconds), and never from after now, which proj refuses.
assert REAL["job"]("sam/r", "the task", since=1792800000.7) == "job-9"
assert open(f"{tmp}/proj-args").read().split()[:5] == ["run", "--pipeline", "--app", "workers", "--since"], open(f"{tmp}/proj-args").read()
assert open(f"{tmp}/proj-args").read().split()[5] == "1792800000"
REAL["job"]("sam/r", "the task", since=time.time() + 3600)
assert int(open(f"{tmp}/proj-args").read().split()[5]) <= time.time()
C.project_dir = project_dir
S.CFG.clear()
S.CFG.update(kept)
print("kicks: ok")

# `kick REPO`: only that repo, none of the full pass's hooks or another
# try at what GitHub said no to about a branch, and its owner's lines and
# wake-ups replace that repo's while the rest stay. First it's started
# over if it was remade, then its settings are seen to (free unless due):
# so the first event about a new repo sets it up and starts its baseline
# job, which its settings and known.json then have. The owner's issue is
# kept up to date from it.
fresh_pr_state()
C.NOW = T
C.save("wake.json", {"sam/r": T + 50, "sam/other": T + 60, "sam/gone": T - 1})
C.save("digest-repos.json", {"sam/r": [["Old", "r line", ""]], "sam/other": [["Open over a day", "other line", "x"]]})
C.save("repos.json", {"untouched": True})
seen_repos, published = [], []
kept_fns = (S.R.discover, S.R.restart, S.R.settle_repo, S.R.publish, S.open_prs)
S.R.discover = lambda: [{"repo": "sam/r", "default": "main"}, {"repo": "sam/other", "default": "main"}]
order = []
S.R.restart = lambda r, known, failed: order.append(("restart", r["repo"]))
S.R.settle_repo = lambda r, known: order.append(("settle", r["repo"])) or r["repo"] not in known
S.R.publish = published.append


def kicked_prs(owner, name):
    seen_repos.append(f"{owner}/{name}")
    return info, [mkpr()]


S.open_prs, argv = kicked_prs, sys.argv
open(UPKEEP_RAN, "w").close()
sys.argv = ["steward", "kick", "Sam/R"]  # as burrow-events spells it, or not
C.WAKE.clear()
C.save("known.json", [])
NEW_JOBS = []
C.job, job = (lambda repo, task, clone=False: NEW_JOBS.append((repo, task, clone)) or "job-new"), C.job
S.CFG.update(baselineJobs=True, baselineTask="REPO is new")
S.main()
S.CFG["baselineJobs"] = False
del S.CFG["baselineTask"]
C.job = job
sys.argv = argv
S.R.discover, S.R.restart, S.R.settle_repo, S.R.publish, S.open_prs = kept_fns
assert seen_repos == ["sam/r"], seen_repos
assert order == [("restart", "sam/r"), ("settle", "sam/r")] and NEW_JOBS == [("sam/r", "sam/r is new", True)], \
    (order, NEW_JOBS)
assert C.load("known.json", None) == ["sam/r"]
# The branch watch leaves every open PR's branches, its base as well as its head
# (deleting a base closes the PRs on it), and a kicked pass's doesn't try
# again what GitHub said no to.
ran = [json.loads(x) for x in open(UPKEEP_RAN)]
watched = [(x[1], x[x.index("--default") + 1], "--full" in x, x[-1]) for x in ran if x[0] == "branches"]
assert watched == [("sam/r", "main", False, ["feat", "main"])], ran
assert not [x for x in ran if x[0] == "hooks"], ran
assert C.load("repos.json", None) == {"untouched": True}
assert C.load("wake.json", None) == {"sam/r": T + 3 * 60 + 1, "sam/other": T + 60}, C.load("wake.json", None)
assert C.load("digest-repos.json", None) == {"sam/r": [], "sam/other": [["Open over a day", "other line", "x"]]}
assert published == [[("Open over a day", "other line", "x")]], published
print("kick: ok")

# A repo that can't be started over as new, though it was remade, is left
# alone that pass, its dashboard and flaky tests too: nothing of the deleted
# one's may act on it. Its next pass, five minutes on, tries again.
fresh_pr_state()
C.NOW = T
seen_repos, watched = [], []
kept_fns = (S.R.discover, S.R.restart, S.R.settle_repo, S.R.publish, S.open_prs, S.upkeep)
S.R.discover = lambda: [{"repo": "sam/r", "default": "main"}]
S.R.restart = lambda r, known, failed: (_ for _ in ()).throw(RuntimeError("proj stop r-1: couldn't"))
S.R.settle_repo = lambda r, known: (_ for _ in ()).throw(AssertionError("settled a repo not started over"))
S.R.publish = lambda lines: None
S.open_prs = kicked_prs
S.upkeep = lambda repo, mine: watched.append(repo)
sys.argv = ["steward", "kick", "sam/r"]
C.WAKE.clear()
S.main()
sys.argv = argv
S.R.discover, S.R.restart, S.R.settle_repo, S.R.publish, S.open_prs, S.upkeep = kept_fns
assert seen_repos == [] and watched == [], (seen_repos, watched)
assert C.load("wake.json", None) == {"sam/r": T + 5 * 60}, C.load("wake.json", None)
print("remade, not started over: ok")

# A repo made empty (no default branch yet): its pass sees to its settings
# and hook, so its first push is heard, and nothing else: no PRs to read, no
# baseline job yet (nothing to open a PR on), and it isn't known until then.
fresh_pr_state()
settled, seen_repos, NEW_JOBS = [], [], []
kept_fns = (S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs)
S.R.discover = lambda: [{"repo": "sam/new", "default": None}]
S.R.settle_repo = lambda r, known: settled.append(r["repo"]) or r["repo"] not in known
S.R.publish = lambda *a: None
S.open_prs = kicked_prs
C.job, job = (lambda repo, task, clone=False: NEW_JOBS.append(repo) or "job-new"), C.job
S.CFG.update(baselineJobs=True, baselineTask="REPO is new")
C.save("known.json", [])
for args in (["steward", "kick", "sam/new"], ["steward"]):
    sys.argv = args
    S.main()
S.CFG["baselineJobs"] = False
del S.CFG["baselineTask"]
C.job, sys.argv = job, argv
S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs = kept_fns
assert settled == ["sam/new", "sam/new"] and seen_repos == [] and NEW_JOBS == [], (settled, seen_repos, NEW_JOBS)
assert C.load("known.json", None) == [], C.load("known.json", None)
print("empty repos: ok")

# A pass GitHub's rate limit turns away books its repos' next pass for when
# the limit resets (`gh api rate_limit`, free), not five minutes on, the
# event runner's backoff or the hourly pass: none before then could do
# anything, one then waits on nothing. Wherever the pass meets it: finding
# its repos, in a repo's PRs, or after its wake-ups were saved (the owner's
# issue). Every wake-up waits for it, booked before or by another pass.
fake = os.path.join(tmp, "limited-bin")
os.makedirs(fake)
with open(f"{fake}/gh", "w") as f:
    f.write(f"""#!/bin/sh
if [ "$1 $2" = "api rate_limit" ]; then echo {T + 1800:.0f}; exit 0; fi
echo 'gh: API rate limit exceeded for user ID 1. (HTTP 403)' >&2
exit 1
""")
os.chmod(f"{fake}/gh", 0o755)


def limited(*a, **k):
    """discover, open_prs or publish meeting the limit, for sam's repos as each does."""
    with C.acting_for("sam"):
        return REAL_GH("api", "repos/sam/r")


kept_fns = (S.R.discover, S.R.settle_repo, S.open_prs, S.R.publish)
S.R.settle_repo = lambda r, known: False
for where in ("discovery", "prs", "after"):
    fresh_pr_state()
    C.NOW = T
    C.save("wake.json", {"sam/other": T + 60, "Sam/R": T + 9000})
    S.R.discover = limited if where == "discovery" else (lambda: [{"repo": "sam/r", "default": "main"}])
    S.open_prs = limited if where == "prs" else (lambda owner, name: (info, [mkpr()]))
    S.R.publish = limited if where == "after" else (lambda lines: None)
    path, os.environ["PATH"] = os.environ["PATH"], fake + os.pathsep + os.environ["PATH"]
    C.WAKE.clear()
    C.LIMITED.clear()
    # The repo it was for comes back at the reset, whatever it had booked,
    # however it's spelled; a full pass comes back as one, whatever it got to
    # (its settings, hooks and the owner's issue are its own).
    for argv_now in (["steward", "kick", "sam/r"],) + ((["steward"],) if where != "prs" else ()):
        sys.argv = argv_now
        try:
            S.main()
            assert where != "discovery", "found its repos with GitHub turning it away"
        except RuntimeError as e:
            assert isinstance(e, C.Unavailable) and where != "prs", (where, e)
        assert C.LIMITED == [T + 1800], (where, C.LIMITED)
        woke = C.load("wake.json", None)
        if len(argv_now) > 1:
            assert woke == {"sam/r": T + 1800 + 1, "sam/other": T + 1800 + 1}, (where, woke)
        else:  # what the full pass booked for its repos as well, none sooner
            assert woke[""] == T + 1800 + 1 and woke["sam/other"] == T + 1800 + 1, (where, woke)
            assert min(woke.values()) == T + 1800 + 1, (where, woke)
        C.save("wake.json", {"sam/other": T + 60, "Sam/R": T + 9000})
    os.environ["PATH"] = path
sys.argv = argv
S.R.discover, S.R.settle_repo, S.open_prs, S.R.publish = kept_fns
# Without a limit met, the wake-ups are as booked.
C.LIMITED.clear()
fresh_pr_state()
run(mkpr(), 10)
assert C.WAKE == {"sam/r": T + 10 + 3 * 60 + 1}, C.WAKE
print("rate limit: ok")

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
assert ASKED == [("prs", None), ("older reviews", "c-r2")] + [("older comments", "PR_3")] * P.OLDER_PAGES, ASKED
assert len(prs[2]["comments"]["nodes"]) == 1 + P.OLDER_PAGES and prs[2]["comments"]["pageInfo"]["hasPreviousPage"], prs[2]
# Cut short, it's still just an outside contribution.
fresh_pr_state()
long_outside = mkpr()
long_outside["author"] = {"login": "eve"}
long_outside["comments"]["pageInfo"]["hasPreviousPage"] = True
statuses, digest = run(long_outside, 600)
assert "Outside contribution" in statuses and [x[0] for x in digest] == ["Outside contributions"], (statuses, digest)
print("long PRs: ok")
