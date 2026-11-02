"""burrow-upkeep in a pass: each repo's labels, settings and hook, kept by
the binary, the branches nothing needs, deleted after its PRs are seen to, and its Renovate dashboard and flaky tests,
looked at by the binary (its own tests have the rules) as the App for the
repo's owner, what it lists put where the steward sorts it, the passes it
books booked, the Claude jobs it asks for started under their keys and
found by them, and what happens when any of that fails: the pass goes
on."""

import contextlib
import io
import json
import os
import sys
import time

from harness import UPKEEP, C, R, S, tmp, upkeep_config

import waits  # noqa: E402 (harness puts the steward on the path)

T = 1_800_000_000
DAY = 86400

# GitHub, as gh answers burrow-upkeep's calls (`gh api -i -X METHOD PATH`):
# the first answer whose method and path's start match, a 304 for its etag,
# and GraphQL's issues by number. Each call is noted, with its token.
GH, CALLED = f"{tmp}/upkeep-gh.json", f"{tmp}/upkeep-called"
os.makedirs(f"{tmp}/upkeep-bin")
with open(f"{tmp}/upkeep-bin/gh", "w") as f:
    f.write(f"""#!{sys.executable}
import json, os, sys
a = sys.argv[1:]
method, path = a[3], a[4]
etag = next((x.split(": ", 1)[1] for x in a if x.startswith("If-None-Match: ")), None)
body = json.loads(sys.stdin.read()) if "--input" in a else None
with open({CALLED!r}, "a") as f:
    f.write(json.dumps({{"method": method, "path": path, "etag": etag, "token": os.environ.get("GH_TOKEN")}}) + "\\n")
gh = json.load(open({GH!r}))


def answer(status, value, tag=None):
    head = f"HTTP/1.1 {{status}} X\\n" + (f"ETag: {{tag}}\\r\\n" if tag else "") + "Content-Type: application/json\\r\\n\\r\\n"
    sys.stdout.write(head + ("" if value is None else json.dumps(value)))
    sys.exit(0 if status < 300 else 1)


if path == "graphql":
    answer(200, {{"data": {{"repository": {{"issue": gh["issues"][str(body["variables"]["n"])]}}}}}})
for m, start, status, value, tag in gh["answers"]:
    if m == method and path.startswith(start):
        if status == 0:
            sys.stderr.write("error connecting to api.github.com\\n")
            sys.exit(1)
        answer(304, None, tag) if tag and etag == tag else answer(status, value, tag)
sys.stderr.write(f"nothing for {{method}} {{path}}\\n")
sys.exit(1)
""")
os.chmod(f"{tmp}/upkeep-bin/gh", 0o755)
# proj's word on a job (running unless set), and the jobs it has under a
# key, as the job stand-in below starts them; it can't say with "fail".
PROJ_STATES, PROJ_KEYS = f"{tmp}/upkeep-proj.json", f"{tmp}/upkeep-keys.json"
with open(f"{tmp}/upkeep-bin/proj", "w") as f:
    f.write(f"""#!{sys.executable}
import json, sys
if sys.argv[1] == "find":
    keys = json.load(open({PROJ_KEYS!r}))
    if keys.get("fail"):
        sys.stderr.write("proj: /data/dev/jobs: Permission denied\\n")
        sys.exit(1)
    print("\\n".join(keys.get(sys.argv[2], [])))
else:
    print(json.load(open({PROJ_STATES!r})).get(sys.argv[2], "running"))
""")
os.chmod(f"{tmp}/upkeep-bin/proj", 0o755)
os.environ["PATH"] = f"{tmp}/upkeep-bin:{os.environ['PATH']}"


def proj(**states):
    with open(PROJ_STATES, "w") as f:
        json.dump(states, f)


def keys(**kept):
    with open(PROJ_KEYS, "w") as f:
        json.dump(kept, f)


DASHBOARD = """## Errored

 - [ ] <!-- retry-branch=renovate/npm-packages -->chore(deps): update npm packages
"""


def ci(body):
    return {"author": {"login": "github-actions", "__typename": "Bot"}, "body": body,
            "userContentEdits": {"totalCount": 0, "nodes": []}, "comments": {"nodes": []}}


def flaky(n, test, who, kind="User"):
    return {"number": n, "title": f"flaky: {test}", "labels": [{"name": "flaky"}],
            "created_at": "2027-01-15T08:00:00Z", "user": {"login": who, "type": kind}}


def github(*first):
    """What GitHub has: sam/r's dashboard, #7, by sam, with one error on it;
    and two flaky tests, the CI's #4 and #8 by eve, who can push. `first`
    answers ahead of those."""
    with open(GH, "w") as f:
        json.dump({"answers": [*first,
                               ["GET", "search/issues", 200, {"items": [{"number": 7, "title": "Dependency Dashboard", "user": {"login": "sam"}}]}, None],
                               ["GET", "repos/sam/r/issues/7", 200, {"state": "open", "title": "Dependency Dashboard", "user": {"login": "sam"}, "body": DASHBOARD}, 'W/"d"'],
                               ["GET", "repos/sam/r/issues?labels=flaky", 200, [flaky(4, "browser/stream", "github-actions[bot]", "Bot"), flaky(8, "unit/disk", "eve")], None],
                               ["GET", "repos/sam/r/issues/4/events", 200, [], None],
                               ["GET", "repos/sam/r/issues/8/events", 200, [], None],
                               ["GET", "repos/sam/r/collaborators/eve/permission", 200, {"permission": "write"}, None],
                               ["GET", "rate_limit", 200, {"resources": {"core": {"remaining": 0, "reset": T + 1234}}}, None]],
                   "issues": {"4": ci("`browser/stream` flaked in run 1.")}}, f)
    open(CALLED, "w").close()


def called():
    return [json.loads(x) for x in open(CALLED)]


UP = upkeep_config()
JOBS, LOGGED = [], []
NEXT = {}  # what C.job gives next, when set ("jid"): None for no job; or raises ("raise")


def job(repo, task, clone=False, pipeline=True, timeout=None, since=None, key=None):
    """proj run, which keeps the job under its key."""
    JOBS.append({"repo": repo, "task": task, "clone": clone, "pipeline": pipeline, "timeout": timeout, "key": key})
    if "raise" in NEXT:
        raise NEXT.pop("raise")
    jid = NEXT.pop("jid") if "jid" in NEXT else f"up-{len(JOBS)}"
    if jid:
        kept_keys = json.load(open(PROJ_KEYS))
        kept_keys[key] = [jid]
        keys(**kept_keys)
    return jid


kept = (C.job, C.CFG["upkeep"])
C.job = job
C.CFG["upkeep"] = UP
keys()
for f in os.listdir(C.STATE):
    if f.startswith(("dashboard-", "flaky-")) or f == "flaky":
        os.system(f"rm -rf {C.STATE}/{f}")


def look(at, cmd=None):
    """The bridge on sam/r, as a pass calls it, `at` seconds after T: what it
    listed, and what it logged."""
    C.NOW = T + at
    C.WAKE.clear()
    C.LIMITED.clear()
    JOBS.clear()
    S.SEARCHED[0] = 0
    waits.begin()
    C.CFG["upkeep"] = cmd or UP
    mine, out = [], io.StringIO()
    with contextlib.redirect_stdout(out), C.acting_for("sam/r"):
        S.upkeep("sam/r", mine)
    LOGGED[:] = out.getvalue().splitlines()
    return mine


def state(part):
    return json.load(open(f"{C.STATE}/{part}-sam_r.json"))


# The first look: the dashboard is found (one search, counted), its error
# isn't due for a day; the CI's flaky test goes to a Claude job at once,
# project work with its own timeout, in a clone if need be, under its key.
# Eve's is the owner's to look at, listed under the heading the steward
# gives the owner; every call went as the App.
github()
proj()
mine = look(0)
assert mine == [("Flaky tests someone else quarantined", "sam/r#8", "`unit/disk`, by eve")], mine
assert all(x[0] in R.OWNERS for x in mine), mine
assert [(j["pipeline"], j["timeout"], j["clone"]) for j in JOBS] == [(False, 240, True)], JOBS
assert "`browser/stream`: https://github.com/sam/r/issues/4" in JOBS[0]["task"], JOBS
assert JOBS[0]["key"] == f"upkeep/sam/r/flaky/{T}", JOBS
assert state("flaky")["asked"] == [4] and state("flaky")["ask"] == {"key": f"upkeep/sam/r/flaky/{T}", "at": T}, state("flaky")
assert C.WAKE == {"sam/r": T + 245 * 60}, C.WAKE
assert S.SEARCHED == [1], S.SEARCHED
assert {c["token"] for c in called()} == {"ghs_app_sam"}, called()
assert "sam/r" not in waits.FAILED, LOGGED

# A day on: the flaky test's job is found by its key, and handed its
# issue as of when it was asked for; nothing told the binary its id. It's
# still at work, so it's looked at again in a quarter of an hour. The
# dashboard's error is due, a job of the pipeline's with the steward's
# timeout. No search: the dashboard's number is kept, and its read asks
# with its etag.
github()
mine = look(DAY)
assert state("flaky")["issues"]["4"] == {"test": "browser/stream", "handed": T, "job": "up-1"}, state("flaky")
assert [(j["pipeline"], j["timeout"]) for j in JOBS] == [(True, 60)], JOBS
assert "Errored: chore(deps): update npm packages (branch renovate/npm-packages)" in JOBS[0]["task"], JOBS
assert S.SEARCHED == [0] and ["repos/sam/r/issues/7", 'W/"d"'] in [[c["path"], c["etag"]] for c in called()], called()
assert C.WAKE == {"sam/r": T + DAY + 15 * 60}, C.WAKE
# Still queued hours on, the dashboard's job is found, and none other
# starts beside it.
proj(**{"up-1": "queued"})
mine = look(DAY + 5 * 3600)
assert not JOBS and state("dashboard")["items"]["branch renovate/npm-packages"]["job"] == "up-1", (JOBS, state("dashboard"))
assert state("dashboard")["items"]["branch renovate/npm-packages"]["handed"] == T + DAY, state("dashboard")
proj()

# A job that can't start: what it was for is listed for the pipeline, a
# look booked in five minutes, and nothing handed.
for f in ("dashboard", "flaky"):
    os.remove(f"{C.STATE}/{f}-sam_r.json")
keys()  # and the jobs proj had: these looks start at T again
github()
NEXT["jid"] = None
mine = look(0)
NEXT.pop("jid", None)
stuck = [x for x in mine if x[0] not in R.OWNERS]
assert stuck == [("Couldn't start a Claude job", "sam/r#4", "for the flaky test `browser/stream`")], mine
assert C.WAKE == {"sam/r": T + 300}, C.WAKE
assert "handed" not in state("flaky")["issues"]["4"], state("flaky")
assert "sam/r: couldn't start its flaky job (no checkout, or proj refused)" in LOGGED, LOGGED
# proj has no job under its key: the next look asks again, under the same
# key, so a start still under way can't end up beside a second job.
mine = look(60)
assert len(JOBS) == 1 and JOBS[0]["key"] == f"upkeep/sam/r/flaky/{T}", JOBS
assert "handed" not in state("flaky")["issues"]["4"], state("flaky")

# While proj can't say what it has under a key, the part fails, is
# watched, and asks for nothing; once it can, the job is found.
keys(fail=True)
mine = look(90)
assert not JOBS and "sam/r" in waits.FAILED and C.WAKE["sam/r"] == T + 90 + 300, (JOBS, waits.FAILED, C.WAKE)
assert any(f"its flaky tests: whether the job it asked for started (upkeep/sam/r/flaky/{T}): proj find:" in x
           and "Permission denied" in x for x in LOGGED), LOGGED
keys(**{f"upkeep/sam/r/flaky/{T}": ["up-1"]})
mine = look(120)
assert not JOBS and state("flaky")["issues"]["4"]["job"] == "up-1", (JOBS, state("flaky"))

# The steward's clock has a fraction, and this one reads back from the state
# file's JSON a hair off: the next look still finds the job under the key it
# was asked for under, and starts no other.
os.remove(f"{C.STATE}/flaky-sam_r.json")
keys()
github()
at = 1793945621.8617759 - T
look(at)
key = "upkeep/sam/r/flaky/1793945621.8617759"
assert [j["key"] for j in JOBS if "/flaky/" in j["key"]] == [key], JOBS
look(at + 60)
assert not [j for j in JOBS if "/flaky/" in j["key"]], JOBS
assert state("flaky")["issues"]["4"]["job"] == json.load(open(PROJ_KEYS))[key][0], state("flaky")

# A job that raises as it starts (a brief past what exec takes, a clone that
# breaks) is that request's failure: the other part's job starts all the
# same, and the pass goes on. Here the dashboard's error is due a day on,
# alongside the flaky test's.
with open(f"{C.STATE}/dashboard-sam_r.json", "w") as f:
    json.dump({"n": 7, "by": "sam", "items": {"branch renovate/npm-packages": {
        "since": T - 2 * DAY, "section": "Errored", "what": "chore(deps): update npm packages", "failing": True}}}, f)
os.remove(f"{C.STATE}/flaky-sam_r.json")
keys()
github()
NEXT["raise"] = OSError(7, "Argument list too long")
mine = look(0)
assert [j["pipeline"] for j in JOBS] == [True, False], JOBS
assert [j["key"] for j in JOBS] == [f"upkeep/sam/r/{p}/{T}" for p in ("dashboard", "flaky")], JOBS
assert "handed" not in state("dashboard")["items"]["branch renovate/npm-packages"], state("dashboard")
assert ("Couldn't start a Claude job", "sam/r#7", "for what's stuck on Renovate's dashboard") in mine, mine
assert "sam/r" in waits.FAILED and C.WAKE["sam/r"] == T + 300, (waits.FAILED, C.WAKE)
assert "sam/r: its dashboard job didn't start: [Errno 7] Argument list too long" in LOGGED, LOGGED
os.remove(f"{C.STATE}/flaky-sam_r.json")

# A part that fails is logged and watched, and the other part goes on:
# GitHub not answering the dashboard's read leaves the quarantine's job.
github(["GET", "repos/sam/r/issues/7", 502, {}, None])
mine = look(120)
assert len(JOBS) == 1 and not JOBS[0]["pipeline"], JOBS
assert "sam/r" in waits.FAILED and C.WAKE["sam/r"] == T + 120 + 300, (waits.FAILED, C.WAKE)
assert any(x.startswith("sam/r: its dashboard: GET repos/sam/r/issues/7: HTTP 502") for x in LOGGED), LOGGED
# A rate limit is noted, so the pass waits for its reset.
os.remove(f"{C.STATE}/dashboard-sam_r.json")
github(["GET", "search/issues", 403, {"message": "API rate limit exceeded for installation"}, None])
look(180)
assert C.LIMITED == [T + 1234], C.LIMITED

# burrow-upkeep that fails, or runs past its time, is killed and logged,
# and the pass goes on.
mine = look(240, [UPKEEP, f"{tmp}/no-such-config.json"])
assert mine == [] and not JOBS and "sam/r" in waits.FAILED and C.WAKE == {"sam/r": T + 240 + 300}, (mine, C.WAKE)
assert any("its upkeep: burrow-upkeep watch exited 1" in x for x in LOGGED), LOGGED
with open(f"{tmp}/upkeep-slow", "w") as f:
    f.write("#!/bin/sh\nexec sleep 30\n")
os.chmod(f"{tmp}/upkeep-slow", 0o755)
timeout, C.UPKEEP_TIMEOUT = C.UPKEEP_TIMEOUT, 1
began = time.monotonic()
mine = look(300, [f"{tmp}/upkeep-slow"])
C.UPKEEP_TIMEOUT = timeout
assert time.monotonic() - began < 10 and mine == [] and "sam/r" in waits.FAILED, LOGGED
assert any("timed out after 1 seconds" in x for x in LOGGED), LOGGED

# A status run keeps state of its own (in the dir main makes for it), and
# asks as whoever runs it.
was = state("flaky")
github()
os.makedirs(f"{C.STATE}/dry", exist_ok=True)
S.DRY = C.DRY = True
look(360)
S.DRY = C.DRY = False
assert os.path.exists(f"{C.STATE}/dry/flaky-sam_r.json") and state("flaky") == was
assert {c["token"] for c in called()} == {os.environ["GH_TOKEN"]}, called()

# In the steward's pass: each repo is looked at after its PRs, and one that
# fails doesn't stop the rest.
seen, published = [], []
kept_fns = (S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, S.upkeep)
S.R.settle_repo = lambda r, known: False
S.R.discover = lambda: [{"repo": "sam/r", "default": "main"}, {"repo": "sam/q", "default": "main"}]
S.R.publish = published.append
S.open_prs = lambda owner, name: (seen.append(f"prs {owner}/{name}") or {}, [])


def upkeep(repo, mine):
    seen.append(f"upkeep {repo}")
    if repo == "sam/r":
        raise RuntimeError("it broke")
    mine.append(("Still quarantined a day after a Claude job", f"{repo}#4", "x"))


S.upkeep, argv = upkeep, sys.argv
sys.argv = ["steward", "kick", "sam/r", "sam/q"]
C.NOW = T
C.WAKE.clear()
try:
    S.main()
    raise AssertionError("a pass went on past a bridge that raised")
except RuntimeError:
    pass
seen.clear()


def upkeep(repo, mine):  # noqa: F811 (as the bridge fails: within itself)
    seen.append(f"upkeep {repo}")
    if repo == "sam/r":
        waits.failed(repo)
        C.wake(repo, C.NOW + 300)
        return
    mine.append(("Still quarantined a day after a Claude job", f"{repo}#4", "x"))


S.upkeep = upkeep
S.main()
assert seen == ["prs sam/r", "upkeep sam/r", "prs sam/q", "upkeep sam/q"], seen
assert published[-1] == [("Still quarantined a day after a Claude job", "sam/q#4", "x")], published
assert C.load("wake.json", {})["sam/r"] == T + 300, C.load("wake.json", {})
sys.argv = argv
S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, S.upkeep = kept_fns


# Its settings, as a pass keeps them: by the binary (its own tests have the
# rules), as the App, then its hook, which goes by whether they were due.
# GitHub, here: labels there, the settings on (secret scanning aside), no
# rulesets, no protected branch and no hook.
SETTLE = [["PATCH", "repos/sam/r/labels/", 200, {}, None],
          ["GET", "repos/sam/r/hooks?per_page=100", 200, [], None],
          ["POST", "repos/sam/r/hooks", 201, {"id": 9}, None],
          ["GET", "repos/sam/r/rulesets?per_page=100", 200, [], None],
          ["POST", "repos/sam/r/rulesets", 201, {}, None],
          ["GET", "repos/sam/r/branches?protected=true", 200, [], None],
          ["PUT", "repos/sam/r/vulnerability-alerts", 204, None, None],
          ["PATCH", "repos/sam/r", 200, {}, None],
          ["GET", "repos/sam/r", 200, {"allow_auto_merge": True, "delete_branch_on_merge": True, "allow_squash_merge": True}, None]]


def settle(at, *first, private=False, default="main"):
    """settle_repo on sam/r `at` seconds after T, with GitHub as SETTLE has
    it and `first` ahead of that: whether it's new, or what it raised."""
    C.NOW = T + at
    with open(GH, "w") as f:
        json.dump({"answers": [*first, *SETTLE], "issues": {}}, f)
    open(CALLED, "w").close()
    C.CFG["upkeep"] = UP
    out = io.StringIO()
    try:
        with contextlib.redirect_stdout(out), C.acting_for("sam/r"):
            return R.settle_repo({"repo": "sam/r", "private": private, "default": default}, {"sam/r"})
    except RuntimeError as e:
        return e
    finally:
        LOGGED[:] = out.getvalue().splitlines()


def changed():
    return [(c["method"], c["path"]) for c in called() if c["method"] != "GET"]


C.save("repo-sam_r.json", {"databaseId": 2002})
# The first: due, kept, and the hook added, with the hooks' secret.
assert settle(0) is False
st = C.load("repo-sam_r.json", {})
labels = C.CFG["labels"]
assert st["settings"] == T and st["app"] == 7 and st["databaseId"] == 2002 and set(st["labels"]) == {
    labels["hold"], labels["owner"], labels["first"], *labels["waiting"].values()}, st
assert st["hookId"] == 9 and st["hook"], st
assert ("POST", "repos/sam/r/rulesets") in changed() and ("PATCH", "repos/sam/r") in changed(), changed()
assert changed()[-1] == ("POST", "repos/sam/r/hooks"), changed()
assert {c["token"] for c in called()} == {"ghs_app_sam"}, called()
# A minute on, nothing's due, and nothing's asked.
assert settle(60) is False and called() == [], called()
# A private repo has no secret scanning to turn on; an empty one gets no
# ruleset until its first commit.
C.save("repo-sam_r.json", {"databaseId": 2002})
settle(0, private=True, default=None)
assert ("PATCH", "repos/sam/r") not in changed() and ("POST", "repos/sam/r/rulesets") not in changed(), changed()
assert C.load("repo-sam_r.json", {})["empty"], C.load("repo-sam_r.json", {})
# GitHub not saying what the repo is: its hook is still seen to, and then
# the pass hears of it.
C.save("repo-sam_r.json", {"databaseId": 2002})
said = settle(0, ["GET", "repos/sam/r/hooks", 200, [], None], ["GET", "repos/sam/r", 404, {"message": "Not Found"}, None])
assert isinstance(said, RuntimeError) and str(said).startswith("its settings: GET repos/sam/r: HTTP 404"), said
assert ("POST", "repos/sam/r/hooks") in changed() and C.load("repo-sam_r.json", {})["hookId"] == 9, changed()
# A status run changes nothing, and keeps state of its own.
C.save("repo-sam_r.json", {"databaseId": 2002})
S.DRY = C.DRY = True
settle(0)
S.DRY = C.DRY = False
assert changed() == [] and C.load("repo-sam_r.json", {}) == {"databaseId": 2002}, changed()
assert json.load(open(f"{C.STATE}/dry/repo-sam_r.json"))["settings"] == T
print("settling: ok")

# Its branches, as a pass sees to them after its PRs: by the binary (its own
# tests have the rules), as the App, every open PR's left alone. GitHub,
# here: the default (not main), an open PR's branch, and one already in the
# default with no closed PR; git, a stand-in that notes how it was run, and as whom.
BRANCHES = [["GET", "repos/sam/r/branches?per_page=100", 200,
             [{"name": "trunk", "commit": {"sha": "ab" * 20}, "protected": True},
              {"name": "feat", "commit": {"sha": "2" * 40}, "protected": False},
              {"name": "in-main", "commit": {"sha": "1" * 40}, "protected": False}], 'W/"b"'],
            ["GET", "repos/sam/r/pulls?state=closed&sort=updated", 200, [], 'W/"c"'],
            ["GET", "repos/sam/r/pulls?state=closed&per_page=100&head=sam:", 200, [], None],
            ["GET", "repos/sam/r/compare/trunk...", 200, {"ahead_by": 0, "commits": []}, None]]
GIT_SAYS, GIT_RAN = f"{tmp}/upkeep-git-says", f"{tmp}/upkeep-git-ran"
os.makedirs(f"{tmp}/upkeep-git")
with open(f"{tmp}/upkeep-git/git", "w") as f:
    f.write(f"""#!{sys.executable}
import json, os, sys
a = sys.argv[1:]
if a[0] == "init":
    os.makedirs(a[-1], exist_ok=True)
    sys.exit(0)
with open({GIT_RAN!r}, "a") as f:
    f.write(json.dumps({{"args": a, "token": os.environ.get("GH_TOKEN")}}) + "\\n")
code, said = json.load(open({GIT_SAYS!r}))
sys.stderr.write(said)
sys.exit(code)
""")
os.chmod(f"{tmp}/upkeep-git/git", 0o755)
GH013 = "remote: error: GH013: Repository rule violations found for refs/heads/in-main."


def prune(at, full=False, says=(0, ""), cmd=None):
    """The steward's branch watch on sam/r, `at` seconds after T, `feat` an
    open PR's and git saying `says` of a delete: what it listed."""
    C.NOW = T + at
    C.WAKE.clear()
    waits.begin()
    with open(GH, "w") as f:
        json.dump({"answers": BRANCHES, "issues": {}}, f)
    with open(GIT_SAYS, "w") as f:
        json.dump(says, f)
    open(CALLED, "w").close()
    open(GIT_RAN, "w").close()
    C.CFG["upkeep"] = cmd or UP
    mine, out = [], io.StringIO()
    with contextlib.redirect_stdout(out), C.acting_for("sam/r"):
        S.branches("sam/r", "trunk", {"feat"}, mine, full)
    LOGGED[:] = out.getvalue().splitlines()
    return mine


def pushed():
    return [json.loads(x) for x in open(GIT_RAN)]


path = os.environ["PATH"]
os.environ["PATH"] = f"{tmp}/upkeep-git:{path}"
C.save("repo-sam_r.json", {"databaseId": 2002})
# The first look judges the one nothing needs, and books its pass for when
# its wait is over; the open PR's is left alone, and nothing is deleted.
assert prune(0) == [] and pushed() == [], pushed()
assert C.WAKE == {"sam/r": T + 3600}, C.WAKE
st = C.load("repo-sam_r.json", {})
assert st["tips"]["feat"] == {"sha": "2" * 40, "pr": True} and st["tips"]["in-main"]["due"] == T + 3600, st
assert st["databaseId"] == 2002 and {c["token"] for c in called()} == {"ghs_app_sam"}, (st, called())
# An hour on, it's deleted, by git as the App: GitHub refusing it is listed
# for the pipeline, with what it said.
mine = prune(3600, says=(1, GH013))
assert mine == [("Branches GitHub wouldn't let it delete", "sam/r: `in-main`", GH013)], mine
assert [(p["args"][-2:], p["token"]) for p in pushed()] == [(["https://github.com/sam/r.git", ":refs/heads/in-main"], "ghs_app_sam")], pushed()
assert f"--force-with-lease=refs/heads/in-main:{'1' * 40}" in pushed()[0]["args"], pushed()
# Tried again on a full pass alone, which GitHub lets through.
assert prune(3700) == mine and pushed() == [], pushed()
assert prune(3800, full=True) == [] and len(pushed()) == 1, pushed()
# A look whose state can't be saved is the repo's failure, watched; the
# pass goes on.
os.makedirs(f"{C.STATE}/repo-sam_r.json.tmp")
prune(3900)
os.rmdir(f"{C.STATE}/repo-sam_r.json.tmp")
assert any(x.startswith("sam/r: its branches: ") for x in LOGGED), LOGGED
assert "sam/r" in waits.FAILED and C.WAKE["sam/r"] == T + 3900 + 300, (waits.FAILED, C.WAKE)
# A status run deletes nothing, and keeps state of its own.
C.save("repo-sam_r.json", {"databaseId": 2002, "tips": {"in-main": {"sha": "1" * 40, "seen": T - 7200}}})
S.DRY = C.DRY = True
prune(4000)
S.DRY = C.DRY = False
assert pushed() == [] and C.load("repo-sam_r.json", {})["tips"]["in-main"] == {"sha": "1" * 40, "seen": T - 7200}
assert json.load(open(f"{C.STATE}/dry/repo-sam_r.json"))["tips"]["feat"]["pr"]
# With the branch watch off, it asks GitHub nothing.
with open(f"{tmp}/upkeep-off.json", "w") as f:
    json.dump({**json.load(open(f"{tmp}/upkeep.json")), "branches": None}, f)
assert prune(4100, cmd=[UPKEEP, f"{tmp}/upkeep-off.json"]) == [] and called() == [], called()
os.environ["PATH"] = path
C.save("repo-sam_r.json", {})
print("pruning: ok")

C.job, C.CFG["upkeep"] = kept
print("upkeeping: ok")
