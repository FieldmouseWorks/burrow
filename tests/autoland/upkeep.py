"""What a pass keeps up in every repo besides its settings (burrow-upkeep's,
whose tests have them): a repo remade under a deleted one's name started
over as new, and a red default branch's fix job."""

import fcntl
import json
import os
import shutil
import subprocess
import time

from harness import (C, FAIL, FORGOT, GONE, JOBS, R, H, REAL, S, STATES, STOPPED, T0, UNSTOPPABLE, UPKEEP_RAN, V, WROTE, fresh_pr_state, info,
                     mkpr, session_name, stand_in, tmp)

stand_in()
# Its parts are called straight here, so they're for sam's repos as a pass
# has them (steward.run_pass's acting_for).
C._for.append("sam")


# A repo made under the name of one GitHub deleted (GitHub's id for it
# isn't the one the steward kept; for state from before it kept ids, made
# after the steward last settled one of that name, or in that second)
# starts over as new, before anything acts on it. Every clone of the name
# is moved aside, where no job takes it, the new one's own too: which repo
# one is of can't be told. Once they're gone, proj-inbox forgets the
# repo's issues, and then every job of the repo's is stopped (by the repo
# its kick names, or by its folder), wherever it ran; another repo's go
# on. What was kept about the old one goes, and another repo's of a like
# name stays; and it leaves known and failed at once, so it gets its
# baseline job.
src, jobs = f"{tmp}/remade-src", S.CFG["jobsDir"]
S.CFG["projectsDir"] = src
remade = {"repo": "sam/r", "id": 2002, "private": False, "default": "main", "created": "2026-10-24T06:00:00Z"}
MADE = C.ts(remade["created"])
SEQ = iter(range(100))


def clone(d, url="https://github.com/sam/r"):
    """A checkout at src/d whose origin is url."""
    subprocess.run(["git", "init", "-q", f"{src}/{d}"], check=True)
    subprocess.run(["git", "-C", f"{src}/{d}", "remote", "add", "origin", url], check=True)


def job(d, path=None, kick=None):
    """A job proj started in src/d (or in path), with its kick, as proj
    records it; its id."""
    n = next(SEQ)
    jid = f"{d}-20261023-1200{n:02d}-ab{n:02d}"
    os.makedirs(f"{jobs}/{jid}")
    with open(f"{jobs}/{jid}/project", "w") as f:
        f.write(f"{path or f'{src}/{d}'}\n")
    if kick:
        with open(f"{jobs}/{jid}/kick", "w") as f:
            f.write(f"{kick}\nother\n")
    return jid


def old_state():
    fresh_pr_state()
    for d in (src, jobs):
        shutil.rmtree(d, ignore_errors=True)
    os.makedirs(src)
    C.save("repo-sam_r.json", {"settings": C.ts(T0), "app": 7, "labels": ["hold", "needs-owner", "first"],
                               "hook": "x", "hookId": 41})
    C.save("pr-sam_r-1.json", {"fix": "a" * 40})
    C.save("dashboard-sam_r.json", {"items": {"7": {}}})
    C.save("flaky-sam_r.json", {"jobAt": 5})
    C.save("pr-sam_r-x-1.json", {"fix": "b" * 40})
    C.save("repo-sam_r-x.json", {"settings": C.ts(T0)})
    snap = V.snap_name("pr-sam_r-1.json", "a" * 40, "main")
    C.save(f"{V.DONE}/codex--{snap}.json", {"result": "ok"})
    os.makedirs(f"{tmp}/state/{V.SESSIONS}/{session_name('codex', snap)}")
    os.makedirs(f"{tmp}/state/{V.GIT}/sam__r.git/objects")
    C.save("known.json", ["sam/r", "sam/r-x"])
    C.save("baseline-failed.json", {"sam/r": 1, "sam/r-x": 2})


def stamp():
    return time.strftime("%Y%m%dT%H%M%SZ", time.gmtime(C.NOW))


old_state()
assert C.jobs() == []                                      # proj has run none yet
clone("r")                                                 # where the next clone goes
clone("r-2")                                               # another
clone("new", "https://github.com/Sam/R.git")               # the new one's, maybe: whose can't be told
clone("x", "https://github.com/sam/r-x")                   # another repo's
clone("y", "https://github.com/kim/r")                     # another owner's of the same name
old = [job("r"), job("r-2"), job("new"),                   # in clones that go
       job("renamed", kick="autoland/Sam/R")]              # the repo's by its kick, its folder gone
kept = [job("x"), job("y"), job("r-2", kick="proj-inbox/kim/z"),  # other repos', by folder or by kick
        job("z", kick="autoland/sam/r-x"), job("r", "/elsewhere/r")]
os.makedirs(f"{jobs}/r-20261024-070000-abcd")              # one proj is making: it hasn't started
os.makedirs(f"{jobs}/.slots")                              # proj's own
assert (old[3], f"{src}/renamed", {"sam/r"}) in C.jobs() and len(C.jobs()) == 9, C.jobs()
C.NOW = MADE + 3600
known, failed = {"sam/r", "sam/r-x"}, {"sam/r": 1, "sam/r-x": 2}
STOPPED.clear()
FORGOT.clear()
forget = C.forget_issues
C.forget_issues = lambda repo: FORGOT.append((repo, sorted(x for x in os.listdir(src) if x[0] != "."), list(STOPPED)))
R.restart(remade, known, failed)
C.forget_issues = forget
assert FORGOT == [("sam/r", ["x", "y"], [])], FORGOT  # once the clones are gone, before any job is stopped
assert sorted(STOPPED) == sorted(old), STOPPED
assert known == {"sam/r-x"} and failed == {"sam/r-x": 2}, (known, failed)
assert C.load("known.json", None) == ["sam/r-x"] and C.load("baseline-failed.json", None) == {"sam/r-x": 2}
left = sorted(os.listdir(f"{tmp}/state"))
assert not [x for x in left if x.startswith(("pr-sam_r-1", "dashboard-sam_r", "flaky-sam_r", "repo-sam_r."))], left
assert "pr-sam_r-x-1.json" in left and "repo-sam_r-x.json" in left, left
assert os.listdir(f"{tmp}/state/{V.DONE}") == [] and not os.path.exists(f"{tmp}/state/{V.GIT}/sam__r.git")
assert all(x.startswith(GONE) for x in os.listdir(f"{tmp}/state/{V.SESSIONS}")), os.listdir(f"{tmp}/state/{V.SESSIONS}")
assert sorted(os.listdir(src)) == sorted(["x", "y"] + [f".{d}.gone-{stamp()}" for d in ("r", "r-2", "new")])
assert C.project_dir("sam/r") is None, os.listdir(src)
# Then it's settled as new, at once: burrow-upkeep keeps its labels,
# settings and hook, none of them the deleted one's.
WROTE.clear()
open(UPKEEP_RAN, "w").close()
assert R.settle_repo(remade, known) is True
st = C.load("repo-sam_r.json", {})
assert "hookId" not in st and st["databaseId"] == 2002, st
assert [json.loads(x)[:2] for x in open(UPKEEP_RAN)] == [["settle", "sam/r"]]
# Settled now, it isn't started over again: what's kept about it from here
# on stays, its new clone and that clone's jobs too.
C.save("pr-sam_r-2.json", {"new": True})
clone("r")
job("r", kick="autoland/sam/r")
STOPPED.clear()
FORGOT.clear()
R.restart(remade, known, failed)
assert "pr-sam_r-2.json" in os.listdir(f"{tmp}/state") and STOPPED == FORGOT == [] and os.path.isdir(f"{src}/r")
# State from before databaseIds, its node id with it, takes this repo's
# databaseId once restart has passed it, and the node id goes.
st = {**C.load("repo-sam_r.json", {}), "id": "R_new"}
del st["databaseId"]
C.save("repo-sam_r.json", st)
R.settle_repo(remade, known)
st = C.load("repo-sam_r.json", {})
assert st["databaseId"] == 2002 and "id" not in st, st
# Nor is one the steward never settled, or one whose databaseId is the one
# it kept, whenever it was settled; or, for state from before it kept them,
# one made a second or more before the steward last settled it. Settled in
# the second it was made, which comes first can't be told: it starts over.
# A databaseId that isn't this repo's says so even where the old one never
# got its settings. A node id kept before databaseIds were says nothing:
# GitHub has changed their format, so such state goes by the time.
for st, again in ((None, False), ({"databaseId": 2002, "settings": MADE - 86400}, False),
                  ({"databaseId": 1001, "settings": MADE + 86400}, True), ({"databaseId": 1001}, True),
                  ({"settings": MADE + 1}, False), ({"settings": MADE + 0.25}, True),
                  ({"id": "R_old", "settings": MADE + 1}, False), ({"id": "R_new"}, False)):
    old_state()
    if st is None:
        os.remove(f"{tmp}/state/repo-sam_r.json")
    else:
        C.save("repo-sam_r.json", st)
    known = {"sam/r", "sam/r-x"}
    FORGOT.clear()
    R.restart(remade, known, {})
    assert (known == {"sam/r-x"}) == again and (FORGOT == ["sam/r"]) == again and STOPPED == [], (st, known)
    assert ("pr-sam_r-1.json" in os.listdir(f"{tmp}/state")) != again, st
# Its state is found in any case, since GitHub's names don't go by it: a
# deleted Sam/R's, under that spelling, goes when sam/r is made.
old_state()
for name in ("repo-sam_r.json", "pr-sam_r-1.json"):
    os.rename(f"{tmp}/state/{name}", f"{tmp}/state/{name.replace('sam_r', 'Sam_R')}")
os.rename(f"{tmp}/state/{V.GIT}/sam__r.git", f"{tmp}/state/{V.GIT}/Sam__R.git")
# What its reviewers left goes too, as its state spells it.
spelled = V.snap_name("pr-Sam_R-1.json", "a" * 40, "main")
C.save(f"{V.DONE}/codex--{spelled}.json", {"result": "ok"})
known, failed = {"Sam/R", "sam/r-x"}, {"Sam/R": 1}
R.restart(remade, known, failed)
assert known == {"sam/r-x"} and failed == {}, (known, failed)
assert os.listdir(f"{tmp}/state/{V.DONE}") == [f"codex--{V.snap_name('pr-sam_r-1.json', 'a' * 40, 'main')}.json"]
assert not [x for x in os.listdir(f"{tmp}/state") if x.lower().startswith(("repo-sam_r.", "pr-sam_r-1"))]
assert os.listdir(f"{tmp}/state/{V.GIT}") == [], os.listdir(f"{tmp}/state/{V.GIT}")
# One renamed into the name, or moved to it, may be years older than what
# the deleted one left: a clone git made from the name since, and the job
# working in it, are the deleted one's all the same.
old_state()
C.save("repo-sam_r.json", {**C.load("repo-sam_r.json", {}), "databaseId": 1001})
origin = f"{tmp}/remade-origin"
subprocess.run(["git", "init", "-q", origin], check=True)
subprocess.run(["git", "-C", origin, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty",
                "-m", "one"], check=True)
subprocess.run(["git", "-c", f"url.{origin}.insteadOf=https://github.com/sam/r", "clone", "-q",
                "https://github.com/sam/r", f"{src}/r"], check=True)
jid = job("r")
STOPPED.clear()
R.restart({**remade, "created": "2019-01-01T00:00:00Z"}, set(), {})
assert os.listdir(src) == [f".r.gone-{stamp()}"] and STOPPED == [jid], (os.listdir(src), STOPPED)
# Where the repo's next clone goes, every job is the repo's, whether a
# clone of it is there yet or not; unless another repo's clone is there,
# whose they are.
for there in ("sam", None, "kim"):
    old_state()
    if there:
        clone("r", f"https://github.com/{there}/r")
    jid = job("r")
    STOPPED.clear()
    R.restart(remade, set(), {})
    assert STOPPED == ([] if there == "kim" else [jid]), (there, STOPPED)
    assert os.listdir(src) == {"sam": [f".r.gone-{stamp()}"], None: [], "kim": ["r"]}[there], (there, os.listdir(src))
# A step that fails stops the rest, and leaves the settings that say it was
# remade, with the clones it moved: the next pass does it all again, and
# finishes it, their jobs too. A clone whose folder or origin can't be read
# leaves them all, since it may be the repo's; a look the reviewers'
# service is on keeps its entry and its session until it ends, and the
# reset waits for it, whatever code the service runs.
snap = V.snap_name("pr-sam_r-1.json", "a" * 40, "main")
for broken, said in (("stop", "couldn't stop job"), ("origin", "can't read its origin"), ("folder", "symbolic links"),
                     ("forget", "still going"), ("record", "symbolic links"), ("look", "aren't done with"),
                     ("sweep", "aren't done with")):
    C.NOW += 1
    old_state()
    clone("r-old")
    jid = job("r-old")  # only that it ran in a clone moved aside says it's the repo's
    sweep, forget = V.sweep, C.forget_issues
    if broken == "stop":
        UNSTOPPABLE.add(jid)
    elif broken == "origin":
        os.makedirs(f"{src}/half/.git")
    elif broken == "folder":
        os.makedirs(f"{src}/loop")
        os.symlink(".git", f"{src}/loop/.git")
    elif broken == "forget":  # a pass of the inbox's that doesn't end
        C.forget_issues = lambda repo: (_ for _ in ()).throw(RuntimeError(f"proj-inbox forget {repo}: still going"))
    elif broken == "record":  # a job proj keeps that can't be read
        os.symlink("loop-1", f"{jobs}/loop-1")
    elif broken == "look":
        C.save(f"{V.QUEUE}/codex--{snap}.json", {})
        held = open(f"{tmp}/state/{V.QUEUE}/codex--{snap}.json")
        fcntl.flock(held, fcntl.LOCK_SH)
    else:
        V.sweep = lambda *a: [session_name("codex", "pr-sam_r-1-x")]
    known, failed = {"sam/r"}, {}
    STOPPED.clear()
    try:
        R.restart(remade, known, failed)
        raise AssertionError(f"{broken} failing was swallowed")
    except (RuntimeError, OSError) as e:
        assert said in str(e), (broken, e)
    V.sweep, C.forget_issues = sweep, forget
    UNSTOPPABLE.discard(jid)
    assert known == set() and C.load("known.json", None) == [] and "repo-sam_r.json" in os.listdir(f"{tmp}/state")
    assert os.path.isdir(f"{src}/r-old") == (broken in ("origin", "folder")), (broken, os.listdir(src))
    assert STOPPED == ([] if broken in ("stop", "origin", "folder", "forget", "record") else [jid]), (broken, STOPPED)
    if broken == "look":
        assert os.listdir(f"{tmp}/state/{V.QUEUE}") == [f"codex--{snap}.json"]
        assert os.path.isdir(f"{tmp}/state/{V.SESSIONS}/{session_name('codex', snap)}")
        held.close()
    for d in ("half", "loop"):
        shutil.rmtree(f"{src}/{d}", ignore_errors=True)
    if broken == "record":
        os.remove(f"{jobs}/loop-1")
    STOPPED.clear()
    C.NOW += 1
    R.restart(remade, known, failed)
    assert STOPPED == [jid] and "repo-sam_r.json" not in os.listdir(f"{tmp}/state"), (broken, STOPPED)
    assert not os.path.exists(f"{src}/r-old") and not os.path.exists(f"{tmp}/state/{V.GIT}/sam__r.git")
    assert os.listdir(f"{tmp}/state/{V.QUEUE}") == [] and \
        all(x.startswith(GONE) for x in os.listdir(f"{tmp}/state/{V.SESSIONS}")), broken
# Strict, a projectsDir that can't be read raises too, where the listing
# for picking a project finds none.
os.symlink("loop", f"{tmp}/loop")
S.CFG["projectsDir"] = f"{tmp}/loop"
assert list(C.projects()) == []
try:
    list(C.projects(strict=True))
    raise AssertionError("an unreadable projectsDir was taken for an empty one")
except OSError as e:
    assert "symbolic links" in str(e), e
S.CFG["projectsDir"] = src
# proj stop: a job proj no longer has is over already; any other refusal
# is one it may still be at work after.
proj = os.path.join(tmp, "stop-proj")
with open(proj, "w") as f:
    f.write(f'#!/bin/sh\necho "$@" >>{tmp}/stop-args\n'
            '[ "$2" = gone-20261023-120000-abcd ] && { echo "no job $2" >&2; exit 1; }\n'
            '[ "$2" = stuck-20261023-120000-abcd ] && { echo "proj: couldn\'t stop $2" >&2; exit 1; }\nexit 0\n')
os.chmod(proj, 0o755)
S.CFG["proj"], kept_proj = proj, S.CFG["proj"]
assert REAL["stop_job"]("r-20261023-120000-abcd") and REAL["stop_job"]("gone-20261023-120000-abcd")
assert not REAL["stop_job"]("stuck-20261023-120000-abcd"), "a job proj couldn't stop was taken for stopped"
assert open(f"{tmp}/stop-args").read().split("\n")[0] == "stop r-20261023-120000-abcd"
S.CFG["proj"] = kept_proj
# proj-inbox forget: none where there's no inbox; with one, what it
# refuses raises.
inbox = os.path.join(tmp, "forget-inbox")
with open(inbox, "w") as f:
    f.write(f'#!/bin/sh\necho "$@" >>{tmp}/forget-args\n'
            '[ "$2" = kim/busy ] && { echo "proj-inbox: a pass is still going after ten minutes" >&2; exit 1; }\nexit 0\n')
os.chmod(inbox, 0o755)
REAL["forget_issues"]("sam/r")
assert not os.path.exists(f"{tmp}/forget-args")
kept_inbox = S.CFG["inbox"]
S.CFG["inbox"] = {**kept_inbox, "enable": True, "command": inbox}
REAL["forget_issues"]("sam/r")
try:
    REAL["forget_issues"]("kim/busy")
    raise AssertionError("an inbox that didn't forget was taken for one that did")
except RuntimeError as e:
    assert "still going" in str(e), e
assert open(f"{tmp}/forget-args").read().splitlines() == ["forget sam/r", "forget kim/busy"]
S.CFG["inbox"] = kept_inbox
S.CFG["projectsDir"] = f"{tmp}/none"
shutil.rmtree(jobs)
print("remade repos: ok")


# A red default branch gets one Claude job, and nothing more while it works
# or a fix is open; a second if it ends with the branch still red, then the
# owner. Green again, it starts over.
def default(state, sha="c" * 40):
    return {"defaultBranchRef": {"name": "main", "target": {"oid": sha, "statusCheckRollup": {"state": state}}}}


GONE = "Not needed: main is green again without it"
WAIT = "Waiting for CI on main: it lands only while main is red"
repo_r = {"repo": "sam/r", "private": False, "default": "main"}
JOBS.clear()
mine = []
assert R.watch_default(repo_r, default("SUCCESS"), [], mine) == {}
assert R.watch_default(repo_r, default("PENDING"), [], mine) == {}
assert R.watch_default(repo_r, {}, [], mine) == {}
assert JOBS == [] and mine == []
assert R.watch_default(repo_r, default("FAILURE"), [], mine) == {}
assert len(JOBS) == 1 and "is red: CI failed on " + "c" * 40 in JOBS[0] and "autoland/red-cccccccccccc" in JOBS[0], JOBS
assert "gh run list --commit " + "c" * 40 in JOBS[0] and "issue and comment" in JOBS[0], JOBS[0]
assert "gh run list --branch main" in JOBS[0] and "with no PR" in JOBS[0], JOBS[0]
# It's told to look again before it pushes: main may have moved on and gone
# green, someone may have opened a fix, or the same fix may have landed.
assert "Just before you push, check both again" in JOBS[0] and "git diff origin/main is empty" in JOBS[0], JOBS[0]
assert "starts autoland/red-" in JOBS[0] and "if GitHub has one of that name already" in JOBS[0], JOBS[0]
# A PR it counts as a fix under way is one the watcher counts: ready, from
# the repo itself, by one of the steward's authors. Anyone can open one
# from a fork, and it mustn't stop the job.
assert "isDraft,isCrossRepository,author" in JOBS[0] and "opened by one of sam, app/dependabot, app/renovate, app/workers" in JOBS[0], JOBS[0]
assert "One from a fork, a draft, or anyone else's doesn't count" in JOBS[0], JOBS[0]
assert R.watch_default(repo_r, default("FAILURE"), [], mine) == {}
assert len(JOBS) == 1, JOBS  # still running
STATES["job-1"] = "done"
# Red, a fix of its own lands: nothing holds it.
assert R.watch_default(repo_r, default("FAILURE"), [mkpr(branch="autoland/red-cccccccccccc")], mine) == {}
assert len(JOBS) == 1, JOBS  # its PR is open
# So does anyone's of the steward's named for another red: a fix under way.
assert R.watch_default(repo_r, default("FAILURE"), [mkpr(branch="autoland/red-bbbbbbbbbbbb")], mine) == {}
assert len(JOBS) == 1, JOBS
# One from a fork, a draft or someone else, or to another branch, isn't one.
for pr in (dict(mkpr(branch="autoland/red-cccccccccccc"), isCrossRepository=True),
           mkpr(branch="autoland/red-cccccccccccc", draft=True),
           dict(mkpr(branch="autoland/red-cccccccccccc"), author={"login": "mallory"}),
           mkpr(branch="autoland/red-cccccccccccc", base="dev"),
           mkpr(branch="autoland/redcccccccccccc")):
    st_kept = C.load("repo-sam_r.json", {})
    R.watch_default(repo_r, default("FAILURE"), [pr], mine)
    R.watch_default(repo_r, default("FAILURE"), [pr], mine)
    assert len(JOBS) == 2, (pr, JOBS)
    JOBS.pop()
    C.save("repo-sam_r.json", st_kept)
# Ended with no PR: the first pass to see it looks again in a minute (the
# PR may have gone up just after it read them), the second starts another.
C.WAKE.clear()
R.watch_default(repo_r, default("ERROR", sha="d" * 40), [], mine)
assert len(JOBS) == 1 and C.WAKE.get("sam/r") == C.NOW + 60, (JOBS, C.WAKE)
R.watch_default(repo_r, default("ERROR", sha="d" * 40), [], mine)
assert len(JOBS) == 2 and "d" * 40 in JOBS[1], JOBS
STATES["job-2"] = "done"
R.watch_default(repo_r, default("FAILURE", sha="d" * 40), [], mine)
assert len(JOBS) == 2 and mine == [], mine
R.watch_default(repo_r, default("FAILURE", sha="d" * 40), [], mine)
assert len(JOBS) == 2 and mine and mine[0][0] == "The default branch is red", mine
mine.clear()
R.watch_default(repo_r, default("SUCCESS"), [], mine)
R.watch_default(repo_r, default("FAILURE"), [], mine)
assert len(JOBS) == 3 and mine == [], (JOBS, mine)
# While CI on main runs (a fix landed, so main moved on), every fix waits
# for it to say, its job at work or not: one landing now could land on top
# of the fix that made it green. Then red, they go on; nothing is closed.
# (step.handle holds one too if main moves on after this read: landed.py.)
WROTE.clear()
fixes = [mkpr(branch="autoland/red-cccccccccccc", number=9), mkpr(branch="autoland/red-dddddddddddd", number=8)]
assert R.watch_default(repo_r, default("PENDING", sha="e" * 40), fixes, mine) == {9: WAIT, 8: WAIT}
STATES[f"job-{len(JOBS)}"] = "done"
assert R.watch_default(repo_r, default("PENDING", sha="e" * 40), fixes, mine) == {9: WAIT, 8: WAIT}
# So they do on any word but red or green: a check expected and not there
# yet, or none at all on the tip.
assert R.watch_default(repo_r, default("EXPECTED", sha="e" * 40), fixes, mine) == {9: WAIT, 8: WAIT}
unsaid = {"defaultBranchRef": {"name": "main", "target": {"oid": "e" * 40, "statusCheckRollup": None}}}
assert R.watch_default(repo_r, unsaid, fixes, mine) == {9: WAIT, 8: WAIT}
assert R.watch_default(repo_r, default("FAILURE", sha="e" * 40), fixes, mine) == {}
assert WROTE == [] and len(JOBS) == 3 and C.load("repo-sam_r.json", {}).get("red"), (WROTE, JOBS)
STATES[f"job-{len(JOBS)}"] = "running"
# Green while its job still works: the red is kept until the job ends, and
# then every fix, no longer needed, is closed.
opened = [mkpr(branch="autoland/red-cccccccccccc", number=9)]
assert R.watch_default(repo_r, default("SUCCESS", sha="e" * 40), opened, mine) == {9: GONE}
assert C.load("repo-sam_r.json", {}).get("red") and WROTE == [], WROTE
STATES[f"job-{len(JOBS)}"] = "done"
# GitHub won't close one: the red is kept, the close tried again soon, and
# the pass still leaves the PRs alone.
FAIL.append("pr close 9")
C.WAKE.clear()
assert R.watch_default(repo_r, default("SUCCESS", sha="e" * 40), fixes, mine) == {9: GONE, 8: GONE}
FAIL.remove("pr close 9")
assert C.load("repo-sam_r.json", {}).get("red") and C.WAKE.get("sam/r"), C.WAKE
WROTE.clear()
assert R.watch_default(repo_r, default("SUCCESS", sha="e" * 40), fixes, mine) == {9: GONE, 8: GONE}
assert [x.split()[:3] for x in WROTE] == [["pr", "close", "9"], ["pr", "close", "8"]], WROTE
assert "fix for ccccccc isn't needed" in WROTE[0] and "fix for ddddddd" in WROTE[1], WROTE
assert not C.load("repo-sam_r.json", {}).get("red"), C.load("repo-sam_r.json", {})
R.watch_default(repo_r, default("FAILURE"), [], mine)
assert len(JOBS) == 4 and mine == [], (JOBS, mine)
# Its job ends green with no PR seen: the red is kept for one more look,
# since its PR may have gone up just after the PRs were read. On that look
# it's there, and closed.
STATES[f"job-{len(JOBS)}"] = "done"
C.WAKE.clear()
assert R.watch_default(repo_r, default("SUCCESS", sha="e" * 40), [], mine) == {}
assert C.load("repo-sam_r.json", {}).get("red") and C.WAKE.get("sam/r") == C.NOW + 60, C.WAKE
WROTE.clear()
assert R.watch_default(repo_r, default("SUCCESS", sha="e" * 40), opened, mine) == {9: GONE}
assert [x.split()[:3] for x in WROTE] == [["pr", "close", "9"]], WROTE
assert not C.load("repo-sam_r.json", {}).get("red"), C.load("repo-sam_r.json", {})
# Green with a fix the steward started no job for (someone of its own fixed
# a red under the name): not needed either, and closed.
WROTE.clear()
assert R.watch_default(repo_r, default("SUCCESS", sha="e" * 40), [fixes[1]], mine) == {8: GONE}
assert [x.split()[:3] for x in WROTE] == [["pr", "close", "8"]], WROTE

# In a pass the watcher goes first, and the steward holds a fix PR it holds
# (passes.py has what holding does) before any other PR, so one with
# settled reviews and green CI can't merge before it's closed.
kept_fns = (S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, S.R.watch_default, H.handle)
order = []
S.R.discover = lambda: [{"repo": "sam/r", "default": "main"}]
S.R.settle_repo = lambda r, known: False
S.R.publish = lambda *a: None
S.open_prs = lambda owner, name: (info, [mkpr(number=10), mkpr(branch="autoland/red-cccccccccccc", number=9)])
S.R.watch_default = lambda r, info, prs, digest: order.append("watch") or {9: GONE}
H.handle = lambda repo, pr, info, digest, held: order.append((pr["number"], held))
S.main()
S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, S.R.watch_default, H.handle = kept_fns
assert order == ["watch", (9, GONE), (10, None)], order

C._for.pop()

print("upkeep: ok")
