"""What a pass keeps up in every repo: the labels, the rules' up-to-date
branches and the App their required status comes from, and a red default
branch's fix job."""

import fcntl
import json
import os
import shutil
import subprocess
import time
import urllib.parse

from harness import (C, FAIL, FORGOT, JOBS, R, H, REAL, S, STATES, STOPPED, T0, UNSTOPPABLE, V, WROTE, fresh_pr_state, info, gh_stub, mkpr,
                     stand_in, tmp)

stand_in()
# Its parts are called straight here, so they're for sam's repos as a pass
# has them (steward.run_pass's acting_for).
C._for.append("sam")

# Every repo gets the hold, needs-owner and first labels, and the waiting
# ones (prs.show_waiting), once a day.
LABELS = ["first", "hold", "needs-owner", "waiting: proof slot", "waiting: release age", "waiting: reviewer"]


def made():
    """The labels made, and each one's description."""
    got = {}
    for x in WROTE:
        if x.startswith("label create "):
            name, _, rest = x.removeprefix("label create ").partition(" -R ")
            got[name] = rest.partition(" --description ")[2]
    return got


fresh_pr_state()
C.NOW = C.ts(T0)
C.save("repo-sam_r.json", {"settings": C.NOW, "app": 7})
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert sorted(made()) == LABELS, WROTE
# A waiting label says what the PR waits on, and that its status says more,
# within the 100 characters GitHub takes.
assert all(len(d) <= 100 for d in made().values()), made()
assert all("review-settled says" in d for n, d in made().items() if n.startswith("waiting: ")), made()
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert WROTE == [], WROTE
# A day on, they're made again with the settings, in case one was deleted.
C.NOW += 86400
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert sorted(made()) == LABELS, WROTE
# A repo made the labels before there were waiting ones: they're made at
# once, not a day on.
C.save("repo-sam_r.json", {**C.load("repo-sam_r.json", {}), "labels": ["hold", "needs-owner", "first"]})
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert sorted(made()) == LABELS, WROTE

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
                               "bypass_actors": [{"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "pull_request"}],
                               "rules": [{"type": y} for y in ("deletion", "non_fast_forward", "pull_request", "required_status_checks")]},
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
# Not on a repo with no commit yet, whose first push it would turn away (the
# owner's too: they pass it only by PR). Its labels and other settings are
# set then, and its first push's pass adds the ruleset at once, not a day on.
C.NOW += 86400
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": None}, {"sam/r"})
assert not [x for x in WROTE if "rulesets" in x] and sorted(made()) == LABELS, WROTE
assert "api -X PUT repos/sam/r/vulnerability-alerts" in WROTE and C.load("repo-sam_r.json", {})["empty"], WROTE
C.NOW += 60
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert [x for x in WROTE if "rulesets" in x] == ["api -X POST repos/sam/r/rulesets --input -"], WROTE
assert "empty" not in C.load("repo-sam_r.json", {}), C.load("repo-sam_r.json", {})
C.gh, C.write = gh_stub, write

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
    os.makedirs(f"{tmp}/state/{V.SESSIONS}/{V.session_name('codex', snap)}")
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
assert all(x.startswith(V.GONE) for x in os.listdir(f"{tmp}/state/{V.SESSIONS}")), os.listdir(f"{tmp}/state/{V.SESSIONS}")
assert sorted(os.listdir(src)) == sorted(["x", "y"] + [f".{d}.gone-{stamp()}" for d in ("r", "r-2", "new")])
assert C.project_dir("sam/r") is None, os.listdir(src)
# Then it's settled as new, at once: its settings, labels and hook.
WROTE.clear()
assert R.settle_repo(remade, known) is True
st = C.load("repo-sam_r.json", {})
assert st["settings"] == C.NOW and st.get("hookId") != 41 and st["databaseId"] == 2002, st
assert sorted(made()) == LABELS, WROTE
assert "api -X PUT repos/sam/r/vulnerability-alerts" in WROTE, WROTE
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
known, failed = {"Sam/R", "sam/r-x"}, {"Sam/R": 1}
R.restart(remade, known, failed)
assert known == {"sam/r-x"} and failed == {}, (known, failed)
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
        V.sweep = lambda *a: [V.session_name("codex", "pr-sam_r-1-x")]
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
        assert os.path.isdir(f"{tmp}/state/{V.SESSIONS}/{V.session_name('codex', snap)}")
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
        all(x.startswith(V.GONE) for x in os.listdir(f"{tmp}/state/{V.SESSIONS}")), broken
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

# The App's own webhook, which hears a repo the moment it's made: its URL
# and the hooks' secret, set as the App (its JWT), again when either
# changes and once a day; one GitHub didn't take is tried again next pass.
CALLS = []


def app_call(method, path, jwt, body=None):
    CALLS.append((method, path, jwt, body))
    if FAIL_APP:
        raise FAIL_APP.pop()
    return {}


FAIL_APP = []
kept_app = (C.app_call, C.app_jwt)
C.app_call, C.app_jwt = app_call, (lambda: "the-jwt")
REAL["app_hook"]()
assert CALLS == [("PATCH", "app/hook/config", "the-jwt", {"url": "https://example.org/burrow/hook", "content_type": "json",
                                                       "secret": "hook-secret", "insecure_ssl": "0"})], CALLS
REAL["app_hook"]()
assert len(CALLS) == 1, CALLS
C.NOW += 86400
REAL["app_hook"]()
assert len(CALLS) == 2, CALLS
with open(f"{tmp}/cred/webhook", "w") as f:
    f.write("new-secret\n")
FAIL_APP.append(C.Unavailable("PATCH app/hook/config: HTTP 502"))
REAL["app_hook"]()
REAL["app_hook"]()
assert len(CALLS) == 4 and CALLS[-1][3]["secret"] == "new-secret", CALLS
REAL["app_hook"]()
assert len(CALLS) == 4, CALLS
# Another App, with the same URL and secret: its own webhook is set on the
# next pass, not a day on.
C.CFG["app"]["id"], kept_id = 8, C.CFG["app"]["id"]
REAL["app_hook"]()
C.CFG["app"]["id"] = kept_id
assert len(CALLS) == 5, CALLS
with open(f"{tmp}/cred/webhook", "w") as f:
    f.write("hook-secret\n")
C.app_call, C.app_jwt = kept_app
print("the App's webhook: ok")

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
def ago(days):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(C.NOW - days * 86400))


# Given up on a day past STALE_DAYS (3: agents open a PR when they push), or
# fresh from today.
old, young = ago(4), ago(1)
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
# What GitHub answers instead: listing the branches, comparing one (by
# name), or archiving one (by name).
LISTING, COMPARE, ARCHIVING = [None], {}, {}


def no(k, why):
    """gh failing with `why`: None with check=False (gh logs it), else what
    gh raises."""
    if not k.get("check", True):
        return None
    raise (C.Unavailable if C.UNAVAILABLE.search(why) else RuntimeError)(f"gh api: {why}")


def branches_gh(*a, **k):
    path = (a[2] if a[1:2] == ("--paginate",) else a[1]) if a[:1] == ("api",) and len(a) > 1 else ""
    if path.startswith("repos/sam/r/branches?"):
        if LISTING[0]:
            return no(k, LISTING[0])
        return "\n".join(f"{n} {sha}" for n, (sha, _, _) in BRANCHES.items())
    if path.startswith("repos/sam/r/compare/main..."):
        name, b = next((n, x) for n, x in BRANCHES.items() if x[0] == path.rpartition("...")[2])
        if name in COMPARE:
            return no(k, COMPARE[name])
        return {"ahead": b[1], "date": b[2]}
    if path.startswith("repos/sam/r/git/ref/"):
        got = ARCHIVED.get("refs/" + urllib.parse.unquote(path.removeprefix("repos/sam/r/git/ref/")))
        if got is None:
            return no(k, "HTTP 404: Not Found")
        return got == a[a.index("--jq") + 1].partition('== "')[2][:40]
    if path.startswith("repos/sam/r/pulls?state=closed"):
        # gh's --paginate --jq: the matching numbers from every page, a line each.
        assert a[1] == "--paginate", a
        name = path.partition("head=sam:")[2]
        LOOKED.append(name)
        prs = CLOSED.get(name, [])
        if prs is None:
            return no(k, "HTTP 403: Resource not accessible by integration")
        want = a[a.index("--jq") + 1].partition('.head.sha == "')[2][:40]
        return "\n".join(str(pr["number"]) for pr in prs if pr["head"]["sha"] == want)
    return gh_stub(*a, **k)


def branches_write(*a, **k):
    if a[:2] != ("api", "repos/sam/r/git/refs"):
        return kept_write(*a, **k)
    f = dict(x.split("=", 1) for x in a[2:][1::2])
    if f["ref"].split("/")[2] in ARCHIVING:
        return no(k, ARCHIVING[f["ref"].split("/")[2]])
    if f["ref"] in ARCHIVED or f["ref"].split("/")[2] in REFUSED:
        return no(k, "HTTP 422: Reference already exists")
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
BRANCHES["recent"] = ("0" * 40, 4, ago(R.STALE_DAYS - 1))
R.sweep_branches(repo_r, {"open"}, [])
assert DELETED == [("in-main", "1" * 40), ("old", "7" * 40), ("renovate/closed", "9" * 40), ("archived", "d" * 40)], DELETED
assert len(ARCHIVED) == 2, ARCHIVED
# A sweep that didn't look at every branch isn't one. GitHub not listing
# them (its rate limit, or a no) leaves the last sweep's time and what it
# couldn't archive, so the next full pass sweeps again, not a day later.
del BRANCHES["recent"]
C.NOW += 86400
last = C.load("repo-sam_r.json", {})
for why in ("API rate limit exceeded for installation", "HTTP 403: Resource not accessible by integration"):
    LISTING[0] = why
    DELETED.clear()
    digest = []
    R.sweep_branches(repo_r, {"open"}, digest)
    assert DELETED == [] and C.load("repo-sam_r.json", {}) == last, (why, DELETED)
    assert [line for _, line, _ in digest] == ["sam/r: `refused`"], (why, digest)
LISTING[0] = None
# Nor does one GitHub stopped answering partway: what it deleted stays
# deleted, and the next pass sweeps the rest. Archiving a branch then isn't
# a branch it couldn't archive.
for where, name, gone in ((COMPARE, "young", ["in-main", "old"]),
                          (ARCHIVING, "refused", ["in-main", "old", "renovate/closed", "archived"])):
    where[name] = "HTTP 502: Bad Gateway"
    DELETED.clear()
    R.sweep_branches(repo_r, {"open"}, [])
    assert [n for n, _ in DELETED] == gone, (name, DELETED)
    assert C.load("repo-sam_r.json", {}) == last, (name, C.load("repo-sam_r.json", {}))
    del where[name]
# A branch GitHub says no about is passed over, and the sweep still counts:
# one with no history in common with the default has no comparison.
BRANCHES["orphan"] = ("0" * 40, 1, old)
COMPARE["orphan"] = "HTTP 404: No common ancestor between main and 0000000"
DELETED.clear()
R.sweep_branches(repo_r, {"open"}, [])
assert DELETED == [("in-main", "1" * 40), ("old", "7" * 40), ("renovate/closed", "9" * 40), ("archived", "d" * 40)], DELETED
assert C.load("repo-sam_r.json", {}) == {"branches": C.NOW, "unkept": ["refused"]}, C.load("repo-sam_r.json", {})
R.delete_branch, C.gh, C.write = kept_delete, gh_stub, kept_write
S.C.CFG["keepBranches"] = []
C._for.pop()

print("upkeep: ok")
