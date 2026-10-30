"""What a pass keeps up in every repo: the labels, the rules' up-to-date
branches and the App their required status comes from, and a red default
branch's fix job."""

import json
import os
import subprocess
import time
import urllib.parse

from harness import (C, FAIL, JOBS, R, H, S, STATES, T0, V, WROTE, fresh_pr_state, info, gh_stub, mkpr, stand_in, tmp)

stand_in()
# Its parts are called straight here, so they're for sam's repos as a pass
# has them (steward.run_pass's acting_for).
C._for.append("sam")

# Every repo gets the hold, needs-owner and first labels, once a day.
fresh_pr_state()
C.NOW = C.ts(T0)
C.save("repo-sam_r.json", {"settings": C.NOW, "app": 7})
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert sorted(x.split()[2] for x in WROTE if x.startswith("label create")) == ["first", "hold", "needs-owner"], WROTE
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert WROTE == [], WROTE
# A day on, they're made again with the settings, in case one was deleted.
C.NOW += 86400
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert sorted(x.split()[2] for x in WROTE if x.startswith("label create")) == ["first", "hold", "needs-owner"], WROTE

# Wherever the repo has rules (its own rulesets and the default branch's
# classic protection), up-to-date branches go off and the steward's status
# is required from its App only (App 7 here): the same context from anyone
# else, the owner's login included, satisfies nothing. Tag rulesets, and
# ones that ask for neither, are left as they are, and so are other checks.
checks = {"type": "required_status_checks", "parameters": {
    "strict_required_status_checks_policy": True, "required_status_checks": [{"context": "gate"}]}}
LISTED = [{"id": 1, "name": "baseline", "target": "branch", "source_type": "Repository"},
          {"id": 2, "name": "gates", "target": "branch", "source_type": "Repository"},
          {"id": 3, "name": "tags", "target": "tag", "source_type": "Repository"},
          {"id": 4, "name": "org", "target": "branch", "source_type": "Organization"}]
gates = {"name": "gates", "target": "branch", "source_type": "Repository", "enforcement": "active",
         "bypass_actors": [{"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always"}],
         "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
         "rules": [{"type": "deletion"}, checks]}
SETTINGS = {
    # gh's pages: the strict one is on the second.
    "repos/sam/r/rulesets?per_page=100": [LISTED[:1], LISTED[1:]],
    "repos/sam/r/rulesets/1": {"name": "baseline", "target": "branch", "source_type": "Repository",
                               "rules": [{"type": "deletion"}]},
    "repos/sam/r/rulesets/2": gates,
    "repos/sam/r/branches?protected=true&per_page=100": [[{"name": "main"}]],
    "repos/sam/r/branches/main": {"protection": {"required_status_checks": {"enforcement_level": "everyone"}}},
    "repos/sam/r/branches/main/protection/required_status_checks": {"strict": True},
}


def settings(*a, **k):
    if a[:1] != ("api",) or a[1:2] == () or a[1] not in SETTINGS:
        return gh_stub(*a, **k)
    if isinstance(SETTINGS[a[1]], Exception):  # gh saying GitHub said no
        raise SETTINGS[a[1]]
    got = json.loads(json.dumps(SETTINGS[a[1]]))  # a copy, as GitHub's answer is
    # A listing comes in pages: all of them with --paginate --slurp, else the first.
    return got if "rulesets?" not in a[1] or "--slurp" in a else got[0]


def sent(*a, **k):
    if "input" in k and ("rulesets" in a[3] or "protection" in a[3]):
        BODIES.append((a[3], json.loads(k["input"])))
    return write(*a, **k)


C.gh = settings
BODIES, write = [], C.write
C.write = sent
C.NOW += 86400
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
puts = [x for x in WROTE if "rulesets" in x]
assert len(puts) == 1 and puts[0].startswith("api -X PUT repos/sam/r/rulesets/2 "), WROTE
# The whole ruleset goes back with only the one setting changed: its name,
# branches and bypass are what they were.
want = json.loads(json.dumps({k: v for k, v in gates.items() if k != "source_type"}))
want["rules"][1]["parameters"]["strict_required_status_checks_policy"] = False
PROTECTION = "repos/sam/r/branches/main/protection/required_status_checks"
assert BODIES == [("repos/sam/r/rulesets/2", want), (PROTECTION, {"strict": False})], BODIES
assert f"api -X PATCH {PROTECTION} --input -" in WROTE, WROTE
# The steward's status, required from anyone, is pinned to the App: in a
# ruleset (integration_id) and in classic protection (app_id). The other
# checks keep their sources: one any App may set stays so (GitHub lists it
# with no app_id, and takes -1 for it).
SETTINGS["repos/sam/r/rulesets/2"]["rules"][1]["parameters"].update(
    strict_required_status_checks_policy=False,
    required_status_checks=[{"context": "gate"}, {"context": "review-settled"}, {"context": "ci", "integration_id": 15368}])
SETTINGS[PROTECTION] = {"strict": False, "checks": [{"context": "gate", "app_id": None},
                                                    {"context": "review-settled", "app_id": None},
                                                    {"context": "ci", "app_id": 15368}]}
C.NOW += 86400
WROTE.clear()
BODIES.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
want = json.loads(json.dumps({k: v for k, v in SETTINGS["repos/sam/r/rulesets/2"].items() if k != "source_type"}))
want["rules"][1]["parameters"]["required_status_checks"][1]["integration_id"] = 7
assert BODIES == [("repos/sam/r/rulesets/2", want),
                  (PROTECTION, {"checks": [{"context": "gate", "app_id": -1}, {"context": "review-settled", "app_id": 7},
                                           {"context": "ci", "app_id": 15368}]})], BODIES
assert C.load("repo-sam_r.json", {})["app"] == 7
# Once pinned, nothing to change. Pinned to another App (the steward's old
# one, say), it's moved to this one.
for x in SETTINGS["repos/sam/r/rulesets/2"]["rules"][1]["parameters"]["required_status_checks"]:
    x.setdefault("integration_id", 7)
SETTINGS[PROTECTION]["checks"] = [{"context": "gate", "app_id": -1}, {"context": "review-settled", "app_id": 7}]
C.NOW += 86400
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert not [x for x in WROTE if "rulesets/" in x or "protection" in x], WROTE
SETTINGS[PROTECTION]["checks"][1]["app_id"] = 99
C.NOW += 86400
WROTE.clear()
BODIES.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert BODIES == [(PROTECTION, {"checks": [{"context": "gate", "app_id": -1}, {"context": "review-settled", "app_id": 7}]})], BODIES
SETTINGS[PROTECTION] = {"strict": False}
# Another App than the one a repo was settled for: its settings are due
# straight away, not a day later. Until its checks are known to be pinned
# (GitHub not saying what the protection's are, say), they're settled for no
# App, and stay due.
C.save("repo-sam_r.json", {**C.load("repo-sam_r.json", {}), "settings": C.NOW, "app": 6})
SETTINGS[PROTECTION] = None
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert "api -X PUT repos/sam/r/vulnerability-alerts" in WROTE and "app" not in C.load("repo-sam_r.json", {}), WROTE
SETTINGS[PROTECTION] = {"strict": False}
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert "api -X PUT repos/sam/r/vulnerability-alerts" in WROTE and C.load("repo-sam_r.json", {})["app"] == 7, WROTE
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert WROTE == [], WROTE
# A plan without rules (a private repo on a free account) has nothing to
# pin. Its rulesets not listed for any other reason, on any repo, is GitHub
# not saying: not pinned, and one settled before isn't any more.
rulesets_kept = SETTINGS["repos/sam/r/rulesets?per_page=100"]
free = RuntimeError("gh api repos/sam/r/rulesets?per_page=100: gh: Upgrade to GitHub Pro or make this repository "
                    "public to enable this feature. (HTTP 403)")
down = C.Unavailable("gh api repos/sam/r/rulesets?per_page=100: gh: HTTP 502")
for private, said, pinned in ((True, free, True), (True, down, False), (False, down, False)):
    SETTINGS["repos/sam/r/rulesets?per_page=100"] = said
    C.save("repo-sam_r.json", {**C.load("repo-sam_r.json", {}), "app": 7, "settings": 0})
    R.settle_repo({"repo": "sam/r", "private": private, "default": "main"}, {"sam/r"})
    assert (C.load("repo-sam_r.json", {}).get("app") == 7) == pinned, (private, said)
SETTINGS["repos/sam/r/rulesets?per_page=100"] = rulesets_kept
# A baseline ruleset GitHub didn't take: not pinned either.
SETTINGS["repos/sam/r/rulesets?per_page=100"] = [[]]
FAIL.append("POST repos/sam/r/rulesets")
C.save("repo-sam_r.json", {**C.load("repo-sam_r.json", {}), "app": 7, "settings": 0})
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
FAIL.remove("POST repos/sam/r/rulesets")
assert "app" not in C.load("repo-sam_r.json", {})
SETTINGS["repos/sam/r/rulesets?per_page=100"] = rulesets_kept
# A repo set to have no baseline ruleset still gets its own rules' turned off.
SETTINGS["repos/sam/r/rulesets/2"]["rules"][1]["parameters"]["strict_required_status_checks_policy"] = True
SETTINGS[PROTECTION] = {"strict": True}
S.C.CFG["repos"]["sam/r"] = {**S.C.CFG["repos"].get("sam/r", {}), "ruleset": False}
C.NOW += 86400
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
del S.C.CFG["repos"]["sam/r"]["ruleset"]
puts = [x for x in WROTE if "rulesets/" in x]
assert len(puts) == 1 and puts[0].startswith("api -X PUT repos/sam/r/rulesets/2 "), WROTE
# Every protected branch's, not just the default one's: a PR can be merged
# into any of them. A name with a slash in it is read and changed by its
# encoded name.
for name in ("release%2F1.0", "old"):
    SETTINGS[f"repos/sam/r/branches/{name}"] = SETTINGS["repos/sam/r/branches/main"]
    SETTINGS[f"repos/sam/r/branches/{name}/protection/required_status_checks"] = {
        "strict": False, "checks": [{"context": "review-settled", "app_id": None}]}
SETTINGS["repos/sam/r/branches?protected=true&per_page=100"] = [[{"name": "main"}, {"name": "release/1.0"}],
                                                                [{"name": "old"}]]
WROTE.clear()
BODIES.clear()
assert R.keep_protection("sam/r")
assert WROTE == [f"api -X PATCH {PROTECTION} --input -",
                 "api -X PATCH repos/sam/r/branches/release%2F1.0/protection/required_status_checks --input -",
                 "api -X PATCH repos/sam/r/branches/old/protection/required_status_checks --input -"], WROTE
assert BODIES[1][1] == BODIES[2][1] == {"checks": [{"context": "review-settled", "app_id": 7}]}, BODIES
# The protected branches not listed: nothing's known.
SETTINGS["repos/sam/r/branches?protected=true&per_page=100"] = None
assert not R.keep_protection("sam/r")
SETTINGS["repos/sam/r/branches?protected=true&per_page=100"] = [[{"name": "main"}]]
# Already off, or no classic checks at all: nothing to change.
SETTINGS["repos/sam/r/rulesets/2"]["rules"][1]["parameters"]["strict_required_status_checks_policy"] = False
SETTINGS["repos/sam/r/branches/main"] = {"protection": {"required_status_checks": {"enforcement_level": "off"}}}
C.NOW += 86400
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert not [x for x in WROTE if "rulesets/" in x or "protection" in x], WROTE
# A baseline from the org isn't the repo's: the steward can't pin what the
# org keeps, so the repo gets its own over it (GitHub enforces both), and
# the org's is left as it is.
SETTINGS["repos/sam/r/rulesets?per_page=100"] = [[{**LISTED[0], "id": 9, "source_type": "Organization"}]]
C.NOW += 86400
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert [x for x in WROTE if "rulesets" in x] == ["api -X POST repos/sam/r/rulesets --input -"], WROTE
# And with none at all, it's added, its required status from the App only.
SETTINGS["repos/sam/r/rulesets?per_page=100"] = [[]]
C.NOW += 86400
WROTE.clear()
BODIES.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert [x for x in WROTE if "rulesets" in x] == ["api -X POST repos/sam/r/rulesets --input -"], WROTE
required = [y for y in BODIES[0][1]["rules"] if y["type"] == "required_status_checks"]
assert required[0]["parameters"]["required_status_checks"] == [{"context": "review-settled", "integration_id": 7}], required
C.gh, C.write = gh_stub, write

# A repo made under the name of one GitHub deleted (made after the steward
# last settled one of that name) starts over as new: what was kept about
# the old one goes (another repo's of a like name stays), its settings,
# labels and hook are set again at once, and it leaves known, so it gets
# its baseline job. The machine's checkout of the old one, whose root commit
# GitHub doesn't have, is moved aside where no job takes it; the new one's
# own checkout stays. GitHub not saying which it is: nothing's forgotten
# yet, and the pass tries again.
src = f"{tmp}/remade-src"
S.CFG["projectsDir"] = src


def checkout(d):
    path = f"{src}/{d}"
    subprocess.run(["git", "init", "-q", path], check=True)
    subprocess.run(["git", "-C", path, "remote", "add", "origin", "https://github.com/sam/r"], check=True)
    subprocess.run(["git", "-C", path, "-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "--allow-empty",
                    "-m", d], check=True)
    return subprocess.run(["git", "-C", path, "rev-list", "--max-parents=0", "HEAD"], capture_output=True, text=True,
                          check=True).stdout.strip()


NEW_ROOT, ANSWER = None, {}


def commits(*a, **k):
    if a[:1] == ("api",) and a[1].startswith("repos/sam/r/commits/"):
        sha = a[1].rpartition("/")[2]
        if sha in ANSWER:
            raise ANSWER[sha]
        if sha != NEW_ROOT:
            raise RuntimeError(f"gh api {a[1]}: gh: No commit found for SHA: {sha} (HTTP 422)")
        return sha
    return gh_stub(*a, **k)


def old_state():
    fresh_pr_state()
    C.save("repo-sam_r.json", {"settings": C.ts(T0), "app": 7, "labels": ["hold", "needs-owner", "first"],
                               "hook": "x", "hookId": 41})
    for name in ("pr-sam_r-1.json", "dashboard-sam_r.json", "flaky-sam_r.json", "pr-sam_r-x-1.json", "repo-sam_r-x.json"):
        C.save(name, {"old": True})
    snap = V.snap_name("pr-sam_r-1.json", "a" * 40, "main")
    C.save(f"{V.DONE}/codex--{snap}.json", {"result": "ok"})
    os.makedirs(f"{tmp}/state/{V.SESSIONS}/{V.session_name('codex', snap)}")
    os.makedirs(f"{tmp}/state/{V.GIT}/sam__r.git")


C.gh = commits
remade = {"repo": "sam/r", "private": False, "default": "main", "created": "2026-10-24T06:00:00Z"}
old_state()
OLD_ROOT = checkout("r")
C.NOW = C.ts(T0) + 7 * 3600
known = {"sam/r", "sam/r-x"}
WROTE.clear()
assert R.settle_repo(remade, known) is True and known == {"sam/r-x"}, known
left = sorted(os.listdir(f"{tmp}/state"))
assert "pr-sam_r-1.json" not in left and "dashboard-sam_r.json" not in left and "flaky-sam_r.json" not in left, left
assert "pr-sam_r-x-1.json" in left and "repo-sam_r-x.json" in left, left
assert os.listdir(f"{tmp}/state/{V.DONE}") == [] and not os.path.exists(f"{tmp}/state/{V.GIT}/sam__r.git")
assert all(x.startswith(V.GONE) for x in os.listdir(f"{tmp}/state/{V.SESSIONS}")), os.listdir(f"{tmp}/state/{V.SESSIONS}")
st = C.load("repo-sam_r.json", {})
assert st["settings"] == C.NOW and st.get("hookId") != 41, st
assert sorted(x.split()[2] for x in WROTE if x.startswith("label create")) == ["first", "hold", "needs-owner"], WROTE
assert "api -X PUT repos/sam/r/vulnerability-alerts" in WROTE, WROTE
aside = [x for x in os.listdir(src) if x.startswith(".r.gone-")]
assert not os.path.exists(f"{src}/r") and len(aside) == 1 and C.project_dir("sam/r") is None, os.listdir(src)
# Settled now, it isn't made again: what's kept about it from here on
# stays. Until its baseline job starts it's still new.
C.save("pr-sam_r-2.json", {"new": True})
assert R.settle_repo(remade, known) is True and known == {"sam/r-x"}
known.add("sam/r")
assert R.settle_repo(remade, known) is False and "pr-sam_r-2.json" in os.listdir(f"{tmp}/state")
# Its job cloned the new one: that checkout stays where it is.
NEW_ROOT = checkout("r")
old_state()
assert R.settle_repo(remade, {"sam/r"}) is True and os.path.isdir(f"{src}/r"), os.listdir(src)
# GitHub not answering, or answering something else: it raises, and
# nothing is forgotten or moved.
for e in (C.Unavailable(f"gh api repos/sam/r/commits/{OLD_ROOT}: gh: HTTP 502"),
          RuntimeError(f"gh api repos/sam/r/commits/{OLD_ROOT}: gh: Not Found (HTTP 404)")):
    old_state()
    os.rename(f"{src}/r", f"{src}/new-r")
    os.rename(f"{src}/{aside[0]}", f"{src}/r")
    ANSWER[OLD_ROOT] = e
    known = {"sam/r"}
    try:
        R.settle_repo(remade, known)
        raise AssertionError(f"{e} was swallowed")
    except RuntimeError as got:
        assert got is e, got
    assert known == {"sam/r"} and os.path.isdir(f"{src}/r") and "pr-sam_r-1.json" in os.listdir(f"{tmp}/state")
    assert C.load("repo-sam_r.json", {})["hookId"] == 41
    os.rename(f"{src}/r", f"{src}/{aside[0]}")
    os.rename(f"{src}/new-r", f"{src}/r")
ANSWER.clear()
# One made before the steward settled it is the same repo, whatever else.
old_state()
assert R.settle_repo({**remade, "created": "2026-10-23T00:00:00Z"}, {"sam/r"}) is False
assert "pr-sam_r-1.json" in os.listdir(f"{tmp}/state") and C.load("repo-sam_r.json", {})["hookId"] == 41
S.CFG["projectsDir"] = f"{tmp}/none"
C.gh = gh_stub
print("remade repos: ok")

# A red default branch gets one Claude job, and nothing more while it works
# or its PR is open; a second if it ends with the branch still red, then the
# owner. Green again, it starts over.
def default(state, sha="c" * 40):
    return {"defaultBranchRef": {"name": "main", "target": {"oid": sha, "statusCheckRollup": {"state": state}}}}


repo_r = {"repo": "sam/r", "private": False, "default": "main"}
JOBS.clear()
mine = []
R.watch_default(repo_r, default("SUCCESS"), [], mine)
R.watch_default(repo_r, default("PENDING"), [], mine)
R.watch_default(repo_r, {}, [], mine)
assert JOBS == [] and mine == []
R.watch_default(repo_r, default("FAILURE"), [], mine)
assert len(JOBS) == 1 and "is red: CI failed on " + "c" * 40 in JOBS[0] and "autoland/red-cccccccccccc" in JOBS[0], JOBS
assert "gh run list --commit " + "c" * 40 in JOBS[0] and "issue and comment" in JOBS[0], JOBS[0]
assert "gh run list --branch main" in JOBS[0] and "with no PR" in JOBS[0], JOBS[0]
R.watch_default(repo_r, default("FAILURE"), [], mine)
assert len(JOBS) == 1, JOBS  # still running
STATES["job-1"] = "done"
R.watch_default(repo_r, default("FAILURE"), [mkpr(branch="autoland/red-cccccccccccc")], mine)
assert len(JOBS) == 1, JOBS  # its PR is open
# One from a fork, a draft or someone else, or to another branch, isn't it.
for pr in (dict(mkpr(branch="autoland/red-cccccccccccc"), isCrossRepository=True),
           mkpr(branch="autoland/red-cccccccccccc", draft=True),
           dict(mkpr(branch="autoland/red-cccccccccccc"), author={"login": "mallory"}),
           mkpr(branch="autoland/red-cccccccccccc", base="dev")):
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
# Green while its job still works: the red is kept until the job ends, and
# then its PR, no longer needed, is closed; another one is left alone.
WROTE.clear()
opened = [mkpr(branch="autoland/red-cccccccccccc", number=9)]
assert R.watch_default(repo_r, default("SUCCESS", sha="e" * 40), opened, mine) == {9}
assert C.load("repo-sam_r.json", {}).get("red") and WROTE == [], WROTE
STATES[f"job-{len(JOBS)}"] = "done"
fixes = [mkpr(branch="autoland/red-cccccccccccc", number=9), mkpr(branch="autoland/red-dddddddddddd", number=8)]
# GitHub won't close it: the red is kept, the close tried again soon, and
# the pass still leaves the PR alone.
FAIL.append("pr close 9")
C.WAKE.clear()
assert R.watch_default(repo_r, default("SUCCESS", sha="e" * 40), fixes, mine) == {9}
FAIL.remove("pr close 9")
assert C.load("repo-sam_r.json", {}).get("red") and C.WAKE.get("sam/r"), C.WAKE
WROTE.clear()
assert R.watch_default(repo_r, default("SUCCESS", sha="e" * 40), fixes, mine) == {9}
assert [x.split()[:3] for x in WROTE] == [["pr", "close", "9"]], WROTE
assert not C.load("repo-sam_r.json", {}).get("red"), C.load("repo-sam_r.json", {})
R.watch_default(repo_r, default("FAILURE"), [], mine)
assert len(JOBS) == 4 and mine == [], (JOBS, mine)
# Its job ends green with no PR seen: the red is kept for one more look,
# since its PR may have gone up just after the PRs were read. On that look
# it's there, and closed.
STATES[f"job-{len(JOBS)}"] = "done"
C.WAKE.clear()
assert R.watch_default(repo_r, default("SUCCESS", sha="e" * 40), [], mine) == set()
assert C.load("repo-sam_r.json", {}).get("red") and C.WAKE.get("sam/r") == C.NOW + 60, C.WAKE
WROTE.clear()
assert R.watch_default(repo_r, default("SUCCESS", sha="e" * 40), opened, mine) == {9}
assert [x.split()[:3] for x in WROTE] == [["pr", "close", "9"]], WROTE
assert not C.load("repo-sam_r.json", {}).get("red"), C.load("repo-sam_r.json", {})

# In a pass the watcher goes first, and the steward holds a fix PR it made
# obsolete (passes.py has what holding does) before any other PR, so one
# with settled reviews and green CI can't merge before it's closed.
kept_fns = (S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, S.R.unhook_gone, S.R.hook_projects,
            S.R.watch_default, H.handle)
order = []
S.R.discover = lambda: [{"repo": "sam/r", "default": "main"}]
S.R.settle_repo = lambda r, known: False
S.R.publish = S.R.unhook_gone = lambda *a: None
S.R.hook_projects = lambda repos: []
S.open_prs = lambda owner, name: (info, [mkpr(number=10), mkpr(branch="autoland/red-cccccccccccc", number=9)])
S.R.watch_default = lambda r, info, prs, digest: order.append("watch") or {9}
H.handle = lambda repo, pr, info, digest, obsolete: order.append((pr["number"], obsolete))
S.main()
(S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, S.R.unhook_gone, S.R.hook_projects,
 S.R.watch_default, H.handle) = kept_fns
assert order == ["watch", (9, True), (10, False)], order

# The daily branch sweep deletes a branch already in the default branch, and
# one a closed PR was closed at, a bot's included (GitHub deletes on merge,
# never on close; the PR keeps the commits). One pushed to since its PR
# closed, an open PR's, a bot's with no closed PR at its tip and a kept one
# stay. One with no PR and no commit for STALE_DAYS is kept under
# refs/archive and deleted (one an earlier sweep archived too); one GitHub
# won't archive stays, listed for the pipeline.
old, young = "2026-09-01T00:00:00Z", "2026-10-23T00:00:00Z"
BRANCHES = {"in-main": ("1" * 40, 0, young), "closed": ("2" * 40, 3, young), "moved": ("3" * 40, 1, young),
            "open": ("4" * 40, 2, old), "renovate/x": ("5" * 40, 1, old), "gh-pages": ("6" * 40, 9, old),
            "old": ("7" * 40, 4, old), "young": ("8" * 40, 4, young), "renovate/closed": ("9" * 40, 1, young),
            "renovate/again": ("a" * 40, 1, young), "archived": ("d" * 40, 2, old), "refused": ("e" * 40, 2, old)}
CLOSED = {"closed": [{"number": 3, "head": {"sha": "2" * 40}}], "moved": [{"number": 4, "head": {"sha": "f" * 40}}],
          # Its match is the 101st closed PR on the name: past gh's first page.
          "renovate/closed": [{"number": n, "head": {"sha": "c" * 40}} for n in range(100, 200)]
          + [{"number": 5, "head": {"sha": "9" * 40}}],
          # Its last PR merged at another tip, and the bot has pushed its next update.
          "renovate/again": [{"number": 6, "head": {"sha": "b" * 40}}]}
LOOKED = []
# What GitHub has under refs/archive, and what it won't take there.
ARCHIVED, REFUSED = {f"refs/archive/archived/{'d' * 40}": "d" * 40}, {"refused"}


def branches_gh(*a, **k):
    path = (a[2] if a[1:2] == ("--paginate",) else a[1]) if a[:1] == ("api",) and len(a) > 1 else ""
    if path.startswith("repos/sam/r/branches?"):
        return "\n".join(f"{n} {sha}" for n, (sha, _, _) in BRANCHES.items())
    if path.startswith("repos/sam/r/compare/main..."):
        b = next(x for x in BRANCHES.values() if x[0] == path.rpartition("...")[2])
        return {"ahead": b[1], "date": b[2]}
    if path.startswith("repos/sam/r/git/ref/"):
        got = ARCHIVED.get("refs/" + urllib.parse.unquote(path.removeprefix("repos/sam/r/git/ref/")))
        if got is None:
            return None  # 404
        return got == a[a.index("--jq") + 1].partition('== "')[2][:40]
    if path.startswith("repos/sam/r/pulls?state=closed"):
        # gh's --paginate --jq: the matching numbers from every page, a line each.
        assert a[1] == "--paginate", a
        name = path.partition("head=sam:")[2]
        LOOKED.append(name)
        prs = CLOSED.get(name, [])
        if prs is None:
            return None
        want = a[a.index("--jq") + 1].partition('.head.sha == "')[2][:40]
        return "\n".join(str(pr["number"]) for pr in prs if pr["head"]["sha"] == want)
    return gh_stub(*a, **k)


def branches_write(*a, **k):
    if a[:2] != ("api", "repos/sam/r/git/refs"):
        return kept_write(*a, **k)
    f = dict(x.split("=", 1) for x in a[2:][1::2])
    if f["ref"] in ARCHIVED or f["ref"].split("/")[2] in REFUSED:
        return None  # gh's failure (already there: a 422), logged
    ARCHIVED[f["ref"]] = f["sha"]
    return {"ref": f["ref"], "object": {"sha": f["sha"]}}


DELETED, kept_delete, kept_write = [], R.delete_branch, C.write
R.delete_branch = lambda repo, name, sha: DELETED.append((name, sha))
S.C.CFG["keepBranches"] = ["gh-pages"]
C.gh, C.write = branches_gh, branches_write
C.save("repo-sam_r.json", {})
digest = []
R.sweep_branches(repo_r, {"open"}, digest)
assert DELETED == [("in-main", "1" * 40), ("closed", "2" * 40), ("old", "7" * 40), ("renovate/closed", "9" * 40),
                   ("archived", "d" * 40)], DELETED
assert ARCHIVED == {f"refs/archive/archived/{'d' * 40}": "d" * 40, f"refs/archive/old/{'7' * 40}": "7" * 40}, ARCHIVED
assert sorted(LOOKED) == ["archived", "closed", "in-main", "moved", "old", "refused", "renovate/again",
                          "renovate/closed", "renovate/x", "young"], LOOKED
assert digest == [(R.UNKEPT, "sam/r: `refused`", "")], digest
# Not again until a day on: what it couldn't archive is all a pass shows meanwhile.
DELETED.clear()
digest = []
R.sweep_branches(repo_r, {"open"}, digest)
assert DELETED == [] and [line for _, line, _ in digest] == ["sam/r: `refused`"], (DELETED, digest)
# GitHub won't say which PRs were closed at it: the branch stays. So does
# one whose last commit is a day short of STALE_DAYS. The archived ones are
# deleted again (the stand-in keeps every branch), already kept.
CLOSED["closed"] = None
C.NOW += 86400
recent = time.gmtime(C.NOW - (R.STALE_DAYS - 1) * 86400)
BRANCHES["recent"] = ("0" * 40, 4, time.strftime("%Y-%m-%dT%H:%M:%SZ", recent))
R.sweep_branches(repo_r, {"open"}, [])
assert DELETED == [("in-main", "1" * 40), ("old", "7" * 40), ("renovate/closed", "9" * 40), ("archived", "d" * 40)], DELETED
assert len(ARCHIVED) == 2, ARCHIVED
R.delete_branch, C.gh, C.write = kept_delete, gh_stub, kept_write
S.C.CFG["keepBranches"] = []
C._for.pop()

print("upkeep: ok")
