"""What a pass keeps up in every repo: the labels, the rulesets' up-to-date
branches, and a red default branch's fix job."""

import json

from harness import (C, FAIL, JOBS, R, S, STATES, T0, WROTE, fresh_pr_state, info, gh_stub, mkpr, stand_in)

stand_in()

# Every repo gets the hold and needs-owner labels, once a day.
fresh_pr_state()
C.NOW = C.ts(T0)
C.save("repo-sam_r.json", {"settings": C.NOW})
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert sorted(x.split()[2] for x in WROTE if x.startswith("label create")) == ["hold", "needs-owner"], WROTE
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert WROTE == [], WROTE
# A day on, they're made again with the settings, in case one was deleted.
C.NOW += 86400
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert sorted(x.split()[2] for x in WROTE if x.startswith("label create")) == ["hold", "needs-owner"], WROTE

# Up-to-date branches go off wherever the repo has them: its own rulesets
# and the default branch's classic protection. Tag rulesets, and ones that
# don't ask for it, are left as they are.
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
    "repos/sam/r/branches/main": {"protection": {"required_status_checks": {"enforcement_level": "everyone"}}},
    "repos/sam/r/branches/main/protection/required_status_checks": {"strict": True},
}


def settings(*a, **k):
    if a[:1] != ("api",) or a[1:2] == () or a[1] not in SETTINGS:
        return gh_stub(*a, **k)
    got = SETTINGS[a[1]]
    # A listing comes in pages: all of them with --paginate --slurp, else the first.
    return got if "rulesets?" not in a[1] or "--slurp" in a else got[0]


C.gh = settings
BODIES, write = [], C.write
C.write = lambda *a, **k: (BODIES.append(json.loads(k["input"])) if "rulesets" in " ".join(a) else None) or write(*a, **k)
C.NOW += 86400
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
puts = [x for x in WROTE if "rulesets" in x]
assert len(puts) == 1 and puts[0].startswith("api -X PUT repos/sam/r/rulesets/2 "), WROTE
# The whole ruleset goes back with only the one setting changed: its name,
# branches and bypass are what they were.
want = json.loads(json.dumps({k: v for k, v in gates.items() if k != "source_type"}))
want["rules"][1]["parameters"]["strict_required_status_checks_policy"] = False
assert BODIES == [want], BODIES
assert "api -X PATCH repos/sam/r/branches/main/protection/required_status_checks -F strict=false" in WROTE, WROTE
# A repo set to have no baseline ruleset still gets its own rules' turned off.
SETTINGS["repos/sam/r/rulesets/2"]["rules"][1]["parameters"]["strict_required_status_checks_policy"] = True
S.C.CFG["repos"]["sam/r"] = {**S.C.CFG["repos"].get("sam/r", {}), "ruleset": False}
C.NOW += 86400
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
del S.C.CFG["repos"]["sam/r"]["ruleset"]
puts = [x for x in WROTE if "rulesets/" in x]
assert len(puts) == 1 and puts[0].startswith("api -X PUT repos/sam/r/rulesets/2 "), WROTE
# A default branch with a slash in its name is read and changed by its
# encoded name.
SETTINGS["repos/sam/r/branches/release%2F1.0"] = SETTINGS["repos/sam/r/branches/main"]
SETTINGS["repos/sam/r/branches/release%2F1.0/protection/required_status_checks"] = {"strict": True}
WROTE.clear()
R.not_strict_protection("sam/r", "release/1.0")
assert WROTE == ["api -X PATCH repos/sam/r/branches/release%2F1.0/protection/required_status_checks -F strict=false"], WROTE
# Already off, or no classic checks at all: nothing to change.
SETTINGS["repos/sam/r/rulesets/2"]["rules"][1]["parameters"]["strict_required_status_checks_policy"] = False
SETTINGS["repos/sam/r/branches/main"] = {"protection": {"required_status_checks": {"enforcement_level": "off"}}}
C.NOW += 86400
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert not [x for x in WROTE if "rulesets/" in x or "protection" in x], WROTE
# A baseline from the org counts as the repo's own: none is added.
SETTINGS["repos/sam/r/rulesets?per_page=100"] = [[{**LISTED[0], "id": 9, "source_type": "Organization"}]]
C.NOW += 86400
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert not [x for x in WROTE if "rulesets" in x], WROTE
# And with none at all, it's added.
SETTINGS["repos/sam/r/rulesets?per_page=100"] = [[]]
C.NOW += 86400
WROTE.clear()
R.settle_repo({"repo": "sam/r", "private": False, "default": "main"}, {"sam/r"})
assert [x for x in WROTE if "rulesets" in x] == ["api -X POST repos/sam/r/rulesets --input -"], WROTE
C.gh, C.write = gh_stub, write

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
            S.R.watch_default, S.handle)
order = []
S.R.discover = lambda: [{"repo": "sam/r", "default": "main"}]
S.R.settle_repo = lambda r, known: False
S.R.publish = S.R.unhook_gone = lambda *a: None
S.R.hook_projects = lambda repos: []
S.open_prs = lambda owner, name: (info, [mkpr(number=10), mkpr(branch="autoland/red-cccccccccccc", number=9)])
S.R.watch_default = lambda r, info, prs, digest: order.append("watch") or {9}
S.handle = lambda repo, pr, info, digest, obsolete: order.append((pr["number"], obsolete))
S.main()
(S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, S.R.unhook_gone, S.R.hook_projects,
 S.R.watch_default, S.handle) = kept_fns
assert order == ["watch", (9, True), (10, False)], order

print("upkeep: ok")
