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

# Branches nothing needs go on what changes them: a push, a PR closing, the
# default moving, or the wait they were given ending. One already in the
# default branch, or at the tip a closed PR was closed at (a bot's included:
# GitHub deletes on merge, never on close; the PR keeps the commits), goes
# once it has sat GRACE at that tip. One with no PR and no commit for
# STALE_DAYS is kept under refs/archive and deleted (one an earlier look
# archived too); one GitHub won't archive stays, listed for the pipeline.
# One pushed to since its PR closed, an open PR's, a bot's with no closed PR
# at its tip, the default and a kept one stay.
def ago(days):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(C.NOW - days * 86400))


# Given up on a day past STALE_DAYS (3: agents open a PR when they push), or
# fresh from today.
old, young = ago(4), ago(1)
BRANCHES = {"main": ("ab" * 20, 0, young), "in-main": ("1" * 40, 0, young), "closed": ("2" * 40, 3, young),
            "moved": ("3" * 40, 1, young), "open": ("4" * 40, 2, old), "renovate/x": ("5" * 40, 1, old),
            "gh-pages": ("6" * 40, 9, old), "old": ("7" * 40, 4, old), "young": ("8" * 40, 4, young),
            "renovate/closed": ("9" * 40, 1, young), "renovate/again": ("a" * 40, 1, young),
            "archived": ("d" * 40, 2, old), "refused": ("e" * 40, 2, old)}
PROTECTED = set()  # the default here isn't, so it's listed with the rest
CLOSED = {"closed": [{"number": 3, "head": {"sha": "2" * 40}, "closed_at": ago(5)}],
          "moved": [{"number": 4, "head": {"sha": "f" * 40}, "closed_at": ago(5)}],
          # Its match is the 101st closed PR on the name: past gh's first page.
          "renovate/closed": [{"number": n, "head": {"sha": "c" * 40}, "closed_at": ago(5)} for n in range(100, 200)]
          + [{"number": 5, "head": {"sha": "9" * 40}, "closed_at": ago(5)}],
          # Its last PR merged at another tip, and the bot has pushed its next update.
          "renovate/again": [{"number": 6, "head": {"sha": "b" * 40}, "closed_at": ago(5)}]}
# The closed PRs as GitHub lists them by their last change: each one's
# branch, tip and repo.
FEED = [{"number": pr["number"], "updated_at": pr["closed_at"], "head": {"ref": name, "sha": pr["head"]["sha"], "repo": {"full_name": "sam/r"}}}
        for name, prs in CLOSED.items() for pr in prs]
LOOKED, FRESH, FED = [], [], []  # the branches whose closed PRs it read; what each listing got (200 or 304)
# What GitHub has under refs/archive, and what it won't take there.
ARCHIVED, REFUSED = {f"refs/archive/archived/{'d' * 40}": "d" * 40}, {"refused"}
# What GitHub answers instead: listing the branches or the closed PRs,
# comparing one (by name), or archiving one (by name).
LISTING, FEEDING, COMPARE, ARCHIVING = [None], [None], {}, {}


def no(k, why):
    """gh failing with `why`: None with check=False (gh logs it), else what
    gh raises."""
    if not k.get("check", True):
        return None
    raise (C.Unavailable if C.UNAVAILABLE.search(why) else RuntimeError)(f"gh api: {why}")


def close(name, number, sha, repo="sam/r"):
    """A PR from `name` (in `repo`) closed at `sha`, now: GitHub's times are
    whole seconds."""
    if repo == "sam/r":  # what head=sam:NAME lists
        CLOSED.setdefault(name, []).append({"number": number, "head": {"sha": sha}, "closed_at": ago(0)})
    FEED.append({"number": number, "updated_at": ago(0), "head": {"ref": name, "sha": sha, "repo": {"full_name": repo}}})


def branches_fresh(path, etag=None):
    if path.startswith("repos/sam/r/pulls?state=closed&sort=updated&direction=desc&per_page=100"):
        if FEEDING[0]:
            no({}, FEEDING[0])
        page = sorted(FEED, key=lambda p: p["updated_at"], reverse=True)[:100]
        tag = json.dumps(page)
        FED.append(304 if etag == tag else 200)
        return None if etag == tag else (tag, page)
    if path != "repos/sam/r/branches?per_page=100":
        return kept_fresh(path, etag)
    if LISTING[0]:
        no({}, LISTING[0])
    # What a listing shows: each branch's tip, not what it's compared to.
    tag = json.dumps([sorted((n, b[0]) for n, b in BRANCHES.items()), sorted(PROTECTED)])
    FRESH.append(304 if etag == tag else 200)
    if etag == tag:
        return None
    return tag, [{"name": n, "commit": {"sha": sha}, "protected": n in PROTECTED} for n, (sha, _, _) in BRANCHES.items()]


def branches_gh(*a, **k):
    path = (a[2] if a[1:2] == ("--paginate",) else a[1]) if a[:1] == ("api",) and len(a) > 1 else ""
    if path.startswith("repos/sam/r/compare/"):
        assert path.partition("compare/")[2].partition("...")[0] == repo_r["default"], path
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
        # gh's --paginate --jq: each match's number and close, from every page, a line each.
        assert a[1] == "--paginate", a
        name = path.partition("head=sam:")[2]
        LOOKED.append(name)
        prs = CLOSED.get(name, [])
        if prs is None or isinstance(prs, str):
            return no(k, prs or "HTTP 403: Resource not accessible by integration")
        want = a[a.index("--jq") + 1].partition('.head.sha == "')[2][:40]
        return "\n".join(f'{pr["number"]} {pr["closed_at"]}' for pr in prs if pr["head"]["sha"] == want)
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


def deleted(repo, name, sha):
    """GitHub deleting it, unless it's one whose push is turned away."""
    DELETED.append((name, sha))
    if name not in UNDELETABLE:
        del BRANCHES[name]


def look(in_use=("open",), full=False):
    """A pass's look at sam/r's branches: what it lists for the pipeline."""
    digest = []
    DELETED.clear(), LOOKED.clear(), FRESH.clear(), FED.clear(), C.WAKE.clear()
    R.watch_branches(repo_r, set(in_use), digest, full)
    return [line for _, line, _ in digest]


def tips():
    return C.load("repo-sam_r.json", {})["tips"]


DELETED, UNDELETABLE = [], set()
kept_delete, kept_write, kept_fresh = R.delete_branch, C.write, C.fresh
R.delete_branch = deleted
S.C.CFG["keepBranches"] = ["gh-pages"]
C.gh, C.write, C.fresh = branches_gh, branches_write, branches_fresh
# The daily sweep's state, which nothing reads now.
C.save("repo-sam_r.json", {"branches": C.NOW - 86400, "unkept": ["x"], "stale": []})
# The first look judges every branch it may delete, and deletes none before
# GRACE: its pass wakes when that's over.
assert look(full=True) == [] and DELETED == [], DELETED
assert sorted(LOOKED) == ["archived", "closed", "in-main", "moved", "old", "refused", "renovate/again",
                          "renovate/closed", "renovate/x", "young"], LOOKED
assert C.WAKE == {"sam/r": C.NOW + R.GRACE}, C.WAKE
assert sorted(C.load("repo-sam_r.json", {})) == ["base", "branchesBy", "branchesEtag", "closedEtag", "closedSince",
                                                 "tips"], C.load("repo-sam_r.json", {})
# That pass, a kicked one: the unchanged listings cost nothing against the
# rate limit, and only what's due is judged again. What goes next is the
# unmerged one from today, STALE_DAYS from its last commit.
C.NOW += R.GRACE
assert look() == ["sam/r: `refused`"]
assert FRESH == FED == [304] and sorted(LOOKED) == ["archived", "closed", "in-main", "old", "refused",
                                                    "renovate/closed"], (FRESH, FED, LOOKED)
assert DELETED == [("in-main", "1" * 40), ("closed", "2" * 40), ("old", "7" * 40), ("renovate/closed", "9" * 40),
                   ("archived", "d" * 40)], DELETED
assert ARCHIVED == {f"refs/archive/archived/{'d' * 40}": "d" * 40, f"refs/archive/old/{'7' * 40}": "7" * 40}, ARCHIVED
assert C.WAKE == {"sam/r": C.ts(young) + R.STALE_DAYS * 86400}, C.WAKE
# Nothing changed since: a pass judges nothing (after the one that sees the
# deletes), and the one GitHub won't archive is all it lists. A full pass
# tries that one again.
look()
assert look() == ["sam/r: `refused`"] and FRESH == [304] and LOOKED == [] and DELETED == [], (FRESH, LOOKED, DELETED)
assert look(full=True) == ["sam/r: `refused`"] and LOOKED == ["refused"], LOOKED
# Its PR closing starts the look: an hour later, it goes. Reopened inside
# that hour, it stays.
close("open", 2, "4" * 40)
look(())
assert DELETED == [] and C.WAKE["sam/r"] == C.NOW + R.GRACE, (DELETED, C.WAKE)
C.NOW += R.GRACE
look()
assert DELETED == [] and tips()["open"] == {"sha": "4" * 40, "pr": True}
look(())
C.NOW += R.GRACE
look(())
assert DELETED == [("open", "4" * 40)], DELETED
# So does a PR opened and closed between two passes (their events came as
# one, or went missing): every pass reads the PRs closed since the last, at
# no cost while there are none. Not one from a fork's branch of that name,
# or closed at another tip than the branch's now.
close("renovate/x", 7, "5" * 40, repo="eve/r")
close("renovate/again", 8, "b" * 40)
look(())
assert FED == [200] and LOOKED == [], (FED, LOOKED)
close("renovate/x", 9, "5" * 40)
look(())
assert FED == [200] and LOOKED == ["renovate/x"] and C.WAKE["sam/r"] == C.NOW + R.GRACE, (FED, LOOKED, C.WAKE)
look(())
assert FED == [304] and LOOKED == [], (FED, LOOKED)
C.NOW += R.GRACE
look(())
assert DELETED == [("renovate/x", "5" * 40)], DELETED
# More closed since the last look than a page holds: every branch is judged
# again, from when it was first seen at its tip, since reading on by page
# number could skip one a reopen moves up a page. Only the one a PR closed
# at gets a new wait, an hour from that close.
BRANCHES["paged"] = ("ee" * 20, 1, young)
look(())
before = {name: tip.get("due") for name, tip in tips().items()}
C.NOW += 1
close("paged", 11, "ee" * 20)
for n in range(1000, 1100):
    C.NOW += 1
    close(f"gone-{n}", n, "0" * 40)
assert look(()) == ["sam/r: `refused`"] and FED == [200] and sorted(LOOKED) == sorted(tips()), (FED, LOOKED)
assert tips()["paged"]["due"] == C.NOW - 100 + R.GRACE, tips()["paged"]
assert {name: tip.get("due") for name, tip in tips().items() if name != "paged"} == \
    {name: due for name, due in before.items() if name != "paged"}, tips()
look(())
assert FED == [304] and LOOKED == [], (FED, LOOKED)
C.NOW += R.GRACE
look(())
assert DELETED == [("paged", "ee" * 20)], DELETED
# GitHub's times are whole seconds, the pass's clock isn't: a PR closed in
# the same second its branch (a bot's, with no wait of its own) was first
# seen still has it judged, and deleted an hour on.
BRANCHES["renovate/quick"] = ("e3" * 20, 1, young)
C.NOW += 0.25
look(())
assert "due" not in tips()["renovate/quick"], tips()
close("renovate/quick", 15, "e3" * 20)
look(())
assert LOOKED == ["renovate/quick"] and tips()["renovate/quick"]["due"] == C.NOW + R.GRACE, (LOOKED, tips())
C.NOW += R.GRACE
look(())
assert DELETED == [("renovate/quick", "e3" * 20)], DELETED
C.NOW += 0.75
# A PR opened and closed between passes, at the tip an older one closed at
# too: its hour runs from the newer close.
BRANCHES["twice"] = ("e4" * 20, 1, young)
CLOSED["twice"] = [{"number": 16, "head": {"sha": "e4" * 20}, "closed_at": ago(5)}]
look(())
C.NOW += 1800
close("twice", 17, "e4" * 20)
look(())
assert LOOKED == ["twice"] and tips()["twice"]["due"] == C.NOW + R.GRACE, (LOOKED, tips()["twice"])
C.NOW += R.GRACE
look(())
assert DELETED == [("twice", "e4" * 20)], DELETED
# A pass that stops partway and is tried again judges the closes it read
# again the same: each branch's hour runs from its PR's close, not from the
# pass, so failing passes can't keep putting it off.
BRANCHES.update({"first": ("f1" * 20, 1, young), "second": ("f2" * 20, 1, young)})
look(())
C.NOW += 1
closing = C.NOW
close("first", 12, "f1" * 20)
close("second", 13, "f2" * 20)
answers = CLOSED["second"]
CLOSED["second"] = "HTTP 502: Bad Gateway"
for _ in range(3):
    C.NOW += 600
    look(())
    assert LOOKED == ["first", "second"] and tips()["first"]["due"] == closing + R.GRACE, (LOOKED, tips()["first"])
CLOSED["second"] = answers
look(())
assert LOOKED == ["first", "second"] and tips()["second"]["due"] == closing + R.GRACE, (LOOKED, tips()["second"])
look(())
assert LOOKED == [], LOOKED  # read to the end once: those closes are behind it
C.NOW = closing + R.GRACE
look(())
assert DELETED == [("first", "f1" * 20), ("second", "f2" * 20)], DELETED
# So does a push: to the tip a closed PR was closed at, it goes an hour on.
BRANCHES["moved"] = ("f" * 40, 1, young)
look(())
assert LOOKED == ["moved"] and DELETED == [], (LOOKED, DELETED)
C.NOW += R.GRACE
look(())
assert DELETED == [("moved", "f" * 40)], DELETED
# A delete that doesn't go through is tried again next pass.
BRANCHES["empty"] = ("0" * 40, 0, old)
UNDELETABLE.add("empty")
look(())
C.NOW += R.GRACE
look(())
assert DELETED == [("empty", "0" * 40)], DELETED
UNDELETABLE.clear()
look(())
assert FRESH == [304] and DELETED == [("empty", "0" * 40)], (FRESH, DELETED)
# The stale one's wait ending: archived and deleted.
C.NOW = C.ts(young) + R.STALE_DAYS * 86400
look(())
assert DELETED == [("young", "8" * 40)] and f"refs/archive/young/{'8' * 40}" in ARCHIVED, DELETED
# The default moving judges every branch again, at the tip it has sat at:
# one merged into it since goes as one already in it, unarchived, without
# waiting out STALE_DAYS. Those with no wait of their own (a bot's) are
# looked at too.
BRANCHES["later"] = ("1a" * 20, 2, ago(0))
look(())
assert tips()["later"]["due"] == C.NOW + R.STALE_DAYS * 86400, tips()["later"]
C.NOW += R.GRACE
BRANCHES.update({"main": ("ba" * 20, 0, young), "later": ("1a" * 20, 0, ago(0))})
look(())
assert FRESH == [200] and sorted(LOOKED) == sorted(tips()) and "renovate/again" in LOOKED, (FRESH, LOOKED)
assert DELETED == [("later", "1a" * 20)] and not [ref for ref in ARCHIVED if "/later/" in ref], (DELETED, ARCHIVED)
look(())
assert LOOKED == [], LOOKED  # the listing without it: the default is where it was
# A new default: the old one is judged like any other branch, against the
# new one, though the listing hasn't changed.
BRANCHES["trunk"] = ("cd" * 20, 1, ago(0))
look(())
repo_r["default"] = "trunk"
look(())
assert FRESH == [200] and "main" in LOOKED and "trunk" not in tips(), (FRESH, LOOKED)
C.NOW += R.GRACE
look(())
assert DELETED == [("main", "ba" * 20)], DELETED
repo_r["default"] = "main"
BRANCHES["main"] = BRANCHES.pop("trunk")
look(())
# GitHub not listing them (its rate limit, or a no), or the PRs closed since,
# changes nothing.
look(())
was = C.load("repo-sam_r.json", {})
for failing in (LISTING, FEEDING):
    for why in ("API rate limit exceeded for installation", "HTTP 403: Resource not accessible by integration"):
        failing[0] = why
        assert look(()) == ["sam/r: `refused`"] and C.load("repo-sam_r.json", {}) == was, why
    failing[0] = None
# Nor does GitHub not answering partway lose a branch or a closed PR: what
# it judged is kept, and the next pass lists both again (no ETag kept) and
# judges the rest. Archiving one then isn't one GitHub wouldn't archive.
BRANCHES.update({"new": ("0" * 40, 1, ago(0)), "newer": ("b" * 40, 1, ago(0))})
close("elsewhere", 10, "0" * 40, repo="eve/r")
COMPARE["newer"] = "HTTP 502: Bad Gateway"
look(())
now = C.load("repo-sam_r.json", {})
assert LOOKED == ["new", "newer"] and (now["branchesEtag"], now["closedEtag"]) == (was["branchesEtag"], was["closedEtag"]), LOOKED
del COMPARE["newer"]
look(())
assert FRESH == FED == [200] and LOOKED == ["newer"], (FRESH, FED, LOOKED)
BRANCHES["quiet"] = ("9" * 40, 2, old)
look(())
C.NOW += R.GRACE
ARCHIVING["quiet"] = "HTTP 502: Bad Gateway"
assert look(()) == ["sam/r: `refused`"] and DELETED == [], DELETED
del ARCHIVING["quiet"]
look(())
assert DELETED == [("quiet", "9" * 40)], DELETED
# A branch GitHub said no about, to its closed PRs or its comparison, waits
# for a full pass to be judged again: its no may be for now (its access) or
# for good (no history in common with the default, which reads the same).
# Once GitHub answers, it's judged as any other, from when it was first seen.
BRANCHES.update({"orphan": ("c" * 40, 1, old), "shy": ("e1" * 20, 1, old), "renovate/shy": ("e2" * 20, 1, old)})
COMPARE.update({"orphan": "HTTP 404: No common ancestor between main and ccccccc",
                "shy": "HTTP 403: Resource not accessible by integration"})
CLOSED["renovate/shy"] = None
look(())
seen = C.NOW
assert DELETED == [] and tips()["orphan"] == {"sha": "c" * 40, "seen": seen, "retry": True}, tips()["orphan"]
assert tips()["shy"]["retry"] and tips()["renovate/shy"]["retry"], tips()
look(())
assert LOOKED == [], LOOKED
del COMPARE["shy"]
CLOSED["renovate/shy"] = [{"number": 11, "head": {"sha": "e2" * 20}, "closed_at": ago(5)}]
C.NOW += R.GRACE
look(full=True)
assert sorted(LOOKED) == ["orphan", "refused", "renovate/shy", "shy"], LOOKED
assert DELETED == [("shy", "e1" * 20), ("renovate/shy", "e2" * 20)] and tips()["orphan"]["retry"], (DELETED, tips())
assert f"refs/archive/shy/{'e1' * 20}" in ARCHIVED and tips()["shy"]["seen"] == seen, ARCHIVED
# A first look that stops partway, tried again: it kept no base, so every
# branch is judged again from when it was first seen (a closed PR's from its
# close), and what it judged keeps its wait.
C.save("repo-sam_r.json", {})
BRANCHES["third"] = ("f3" * 20, 1, young)
COMPARE["third"] = "HTTP 502: Bad Gateway"
look(())
first = C.NOW
assert "third" not in tips() and "closedSince" not in C.load("repo-sam_r.json", {}), tips()
C.NOW += 600
close("renovate/again", 14, "a" * 40)
del COMPARE["third"]
look(())
assert tips()["renovate/again"]["due"] == C.NOW + R.GRACE and tips()["refused"]["seen"] == first, tips()
assert tips()["third"]["seen"] == C.NOW and "closedSince" in C.load("repo-sam_r.json", {}), tips()
# A branch kept no longer is judged, though the listing hasn't changed.
S.C.CFG["keepBranches"] = []
look(())
assert FRESH == [200] and LOOKED == ["gh-pages"], (FRESH, LOOKED)
# More than a page of branches: every page, and no ETag for next time. A
# protected one, the default too, isn't the steward's to delete.
C.fresh = lambda path, etag=None: ("tag", [{"name": f"b{i}", "commit": {"sha": "1" * 40}} for i in range(100)])
C.gh = lambda *a, **k: "\n".join([f"main {'2' * 40} true"] + [f"b{i} {'1' * 40} false" for i in range(101)]) \
    if a[2].startswith("repos/sam/r/branches?") else None
assert R.listed_branches("sam/r", "main", None) == (None, {f"b{i}": "1" * 40 for i in range(101)}, ["main", "2" * 40])
R.delete_branch, C.gh, C.write, C.fresh = kept_delete, gh_stub, kept_write, kept_fresh
S.C.CFG["keepBranches"] = []
C._for.pop()

print("upkeep: ok")
