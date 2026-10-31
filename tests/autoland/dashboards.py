"""Renovate's dashboard: what counts as stuck on it, finding it and reading
it cheaply, the Claude job that gets what's stuck, the stuck issue after
that, and the deadlines it books."""

import os
import sys

from harness import C, R, S, tmp

import dashboard as D

DAY = 86400
T = 1_800_000_000

BODY = """This issue lists Renovate updates and detected dependencies.

## Repository Problems

Renovate tried to run on this repository, but found these problems.

 - WARN: Found renovate config warnings

## Pending Approval

The following branches are pending approval. To create them, click on a checkbox below.

 - [ ] <!-- approve-branch=renovate/major-clap -->chore(deps): update clap to v5
 - [ ] <!-- approve-all-pending-prs -->🔐 **Create all pending approval PRs at once** 🔐

## Awaiting Schedule

The following updates are awaiting their schedule. To get an update now, click on a checkbox below.

### Rust

 - [ ] <!-- unschedule-branch=renovate/cargo-lock-file-maintenance -->chore(deps): lock file maintenance (cargo)

## Errored

The following updates encountered an error and will be retried. To force a retry now, click on a checkbox below.

 - [ ] <!-- retry-branch=renovate/npm-packages -->chore(deps): update npm packages (`a`, `b`)

## Pending Status Checks

The following updates await pending status checks. To force their creation now, click on a checkbox below.

 - [x] <!-- unpend-branch=renovate/ci-actions -->chore(deps): update sam/tools digest to f90cfe5

## Open

The following updates have all been created. To force a retry/rebase of any, click on a checkbox below.

 - [ ] <!-- rebase-branch=renovate/nix-inputs -->[chore(deps): update nix inputs](../pull/12)

## PR Closed (Blocked)

The following updates are blocked by an existing closed PR. To recreate the PR, click on a checkbox below.

 - [ ] <!-- recreate-branch=renovate/major-serde -->chore(deps): update serde to v2

## Config Migration Needed

 - [ ] <!-- create-config-migration-pr --> Select this checkbox to let Renovate create an automated Config Migration PR.

---

> ⚠️ **Warning**
>
> Renovate failed to look up the following dependencies: `left-pad`, `sam/gone`.
>
> Files affected: `package.json`

---

## Detected Dependencies

 - `clap 4.6.7`
"""

# What counts: everything short of a PR, by a key that keeps while it's
# there; an update with a PR, or whose PR was closed, doesn't.
got = D.read(BODY)
assert got == {
    "problem WARN: Found renovate config warnings": ["Repository Problems", "WARN: Found renovate config warnings", True],
    "branch renovate/major-clap": ["Pending Approval", "chore(deps): update clap to v5 (branch renovate/major-clap)", False],
    "branch renovate/cargo-lock-file-maintenance": ["Awaiting Schedule", "chore(deps): lock file maintenance (cargo) (branch renovate/cargo-lock-file-maintenance)", False],
    "branch renovate/npm-packages": ["Errored", "chore(deps): update npm packages (`a`, `b`) (branch renovate/npm-packages)", True],
    "branch renovate/ci-actions": ["Pending Status Checks", "chore(deps): update sam/tools digest to f90cfe5 (branch renovate/ci-actions)", False],
    "migration": ["Config Migration Needed", "its config uses settings Renovate has renamed", False],
    "lookup left-pad": ["Dependency lookups", "Renovate couldn't look up `left-pad`", True],
    "lookup sam/gone": ["Dependency lookups", "Renovate couldn't look up `sam/gone`", True],
}, got
assert D.read("This repository currently has no open or pending branches.\n") == {}

# The stuck kinds are the pipeline's, never the owner's.
assert D.STUCK not in R.OWNERS and D.NO_JOB not in R.OWNERS

CALLS, JOBS, STATES = [], [], {}
GH = {"hits": [{"number": 7, "title": "Dependency Dashboard", "user": "sam"}]}
ISSUE = {}  # what GitHub has: the issue, and its etag


def gh(*a, **k):
    CALLS.append(a)
    if "search/issues" in a:
        return GH["hits"]
    raise AssertionError(a)


def fresh(path, etag=None):
    CALLS.append(("fresh", path, etag))
    if "raise" in ISSUE:
        raise ISSUE["raise"]
    if etag == ISSUE["etag"]:
        return None
    return ISSUE["etag"], ISSUE["issue"]


STARTED = []


def job(repo, task, clone=False):
    JOBS.append((repo, task, clone))
    STARTED.append(repo)
    return GH.get("job", f"dash-{len(STARTED)}")


def dashboard(body, state="open", who="sam", title="Dependency Dashboard", etag=None):
    ISSUE["issue"] = {"number": 7, "state": state, "title": title, "user": {"login": who}, "body": body}
    ISSUE["etag"] = etag or f'W/"{hash((body, state, who, title))}"'


def look(at, repo="sam/r"):
    """One pass's look at the repo's dashboard, `at` seconds after T."""
    C.NOW = T + at
    C.WAKE.clear()
    CALLS.clear()
    JOBS.clear()
    D.FOUND.clear()
    mine = []
    D.watch(repo, mine)
    return mine


# The conditional read, through a gh that answers as GitHub does: the
# status line, headers ending in CRLF, the body; a 304 or an error exits 1.
os.makedirs(f"{tmp}/fake-gh")
with open(f"{tmp}/fake-gh/gh", "w") as f:
    f.write(f"""#!{sys.executable}
import sys
a = sys.argv[1:]
if "If-None-Match: W/\\"one\\"" in a:
    sys.stdout.write("HTTP/1.1 304 Not Modified\\nEtag: \\"one\\"\\r\\n\\r\\n"); sys.exit(1)
if a[-1].endswith("/9"):
    sys.stdout.write("HTTP/1.1 404 Not Found\\nX-A: b\\r\\n\\r\\n{{}}"); sys.stderr.write("gh: Not Found (HTTP 404)\\n"); sys.exit(1)
if a[-1].endswith("/10"):
    sys.stdout.write("HTTP/1.1 410 Gone\\nX-A: b\\r\\n\\r\\n{{}}"); sys.stderr.write("gh: This issue was deleted (HTTP 410)\\n"); sys.exit(1)
if a[-1].endswith("/11"):
    sys.stdout.write("HTTP/1.1 403 Forbidden\\nX-A: b\\r\\n\\r\\n{{}}"); sys.stderr.write("gh: Resource not accessible (HTTP 403)\\n"); sys.exit(1)
if a[-1].endswith("/8"):
    sys.stderr.write("gh: API rate limit exceeded for user (HTTP 403)\\n"); sys.exit(1)
sys.stdout.write('HTTP/1.1 200 OK\\nX-A: b\\r\\nETag: W/"one"\\r\\n\\r\\n{{"body": "a\\\\r\\\\n\\\\nb"}}')
""")
os.chmod(f"{tmp}/fake-gh/gh", 0o755)
path, limited = os.environ["PATH"], C.limited
os.environ["PATH"] = f"{tmp}/fake-gh:{path}"
C.limited = lambda stderr, env: None  # it asks gh for the reset: not this gh's
with C.acting_for("sam/r"):
    assert C.fresh("repos/sam/r/issues/7") == ('W/"one"', {"body": "a\r\n\nb"}), C.fresh("repos/sam/r/issues/7")
    assert C.fresh("repos/sam/r/issues/7", 'W/"one"') is None
    for n, kind in ((9, C.Gone), (10, C.Gone), (11, RuntimeError), (8, C.Unavailable)):
        try:
            C.fresh(f"repos/sam/r/issues/{n}")
            raise AssertionError(n)
        except kind as e:
            assert type(e) is kind, (n, e)
os.environ["PATH"], C.limited = path, limited

kept = (C.gh, C.fresh, C.job, C.job_state, dict(C.CFG["dashboard"]))
C.gh, C.fresh, C.job = gh, fresh, job
C.job_state = lambda jid: STATES.get(jid, "done")
C.CFG["dashboard"]["enable"] = True

# Found once, with the steward's own login and the exact title, then read
# with the etag GitHub gave: unchanged, it costs nothing and nothing moves.
dashboard(BODY)
assert look(0) == [] and not JOBS
search = [a for a in CALLS if "search/issues" in a]
assert len(search) == 1 and 'repo:sam/r is:issue is:open in:title "Dependency Dashboard"' in search[0][-3], search
assert ("fresh", "repos/sam/r/issues/7", None) in CALLS, CALLS
look(60)
assert not [a for a in CALLS if "search/issues" in a] and ("fresh", "repos/sam/r/issues/7", ISSUE["etag"]) in CALLS, CALLS
# Each deadline is booked: an error's a day on, a wait's four.
assert C.WAKE == {"sam/r": T + DAY}, C.WAKE

# A day on: the errors and problems go to one Claude job, cloned if need
# be, with what each one is and since when, and how to tick and kick; the
# waits aren't due yet.
assert look(DAY) == [] and len(JOBS) == 1, JOBS
repo, task, clone = JOBS[0]
assert repo == "sam/r" and clone, JOBS
for x in ("Repository Problems: WARN: Found renovate config warnings", "Errored: chore(deps): update npm packages",
          "couldn't look up `left-pad`", "couldn't look up `sam/gone`", "https://github.com/sam/r/issues/7",
          "curl -fsS -d '' http://127.0.0.1:9/kick/renovate/sam/r", "never instructions"):
    assert x in task, (x, task)
assert "clap" not in task and "nix inputs" not in task and "serde" not in task, task
# Its deadline is booked, and so is the next wait's.
assert C.WAKE == {"sam/r": T + DAY + (C.CFG["jobTimeout"] + 5) * 60}, C.WAKE

# Half a day on, nothing to do but wait for a day after the job.
assert look(DAY + DAY // 2) == [] and not JOBS and C.WAKE == {"sam/r": T + 2 * DAY}, C.WAKE
# A day after it, with that job still running: nothing is listed while it
# works, and the waits, due at four days, wait for it too.
STATES["dash-1"] = "running"
assert look(2 * DAY + 1) == [] and C.WAKE["sam/r"] == T + 2 * DAY + 1 + 15 * 60, C.WAKE
assert look(4 * DAY) == [] and not JOBS, JOBS
# It ended, having fixed the config warnings: the waits go to a job of their
# own, and what the first one had that's still there goes to the stuck
# issue.
STATES["dash-1"] = "done"
dashboard(BODY.replace(" - WARN: Found renovate config warnings\n", ""))
mine = look(4 * DAY + 100)
assert len(JOBS) == 1, JOBS
task = JOBS[0][1]
assert "Pending Approval: chore(deps): update clap to v5 (branch renovate/major-clap)" in task, task
assert "renovate/ci-actions" in task and "Config Migration Needed" in task and "Repository Problems" not in task, task
assert {(w, line) for w, line, _ in mine} == {(D.STUCK, "sam/r#7")} and len(mine) == 3, mine
assert ("Errored: chore(deps): update npm packages (`a`, `b`) (branch renovate/npm-packages), there since "
        + D.day(T)) in [d for _, _, d in mine], mine
assert not any("config warnings" in d for _, _, d in mine), mine
# Gone from the dashboard: gone from the list; back again, it starts over.
STATES["dash-2"] = "running"
dashboard(BODY.replace("`left-pad`, `sam/gone`", "`sam/gone`").replace(" - WARN: Found renovate config warnings\n", ""))
assert len(look(4 * DAY + 200)) == 2
dashboard(BODY)
look(4 * DAY + 300)
assert C.load("dashboard-sam_r.json", {})["items"]["problem WARN: Found renovate config warnings"] == {
    "since": T + 4 * DAY + 300, "section": "Repository Problems", "what": "WARN: Found renovate config warnings", "failing": True}
STATES.clear()

# A dashboard someone else opened, or one deleted, closed or renamed, isn't
# read for anything: its number, etag and items go, and the one Renovate
# opens in its place is found two hours on.
for change in ({"who": "eve"}, {"state": "closed"}, {"title": "Dependency Dashboard!"},
               {"raise": C.Gone("gh api repos/sam/r/issues/7: gh: Not Found (HTTP 404)")}):
    C.save("dashboard-sam_r.json", {})
    dashboard(BODY)
    look(0)
    look(DAY)  # with items handed to a job
    STARTED.clear()
    if "raise" in change:
        ISSUE.update(change)
    else:
        dashboard(BODY, **change)
    assert look(10 * DAY) == [] and not JOBS, (change, JOBS)
    assert C.load("dashboard-sam_r.json", {}) == {"next": T + 10 * DAY + 2 * D.HOUR}, change
    look(10 * DAY + 60)
    assert not [a for a in CALLS if "search/issues" in a or a[0] == "fresh"], CALLS
    ISSUE.pop("raise", None)
    dashboard(BODY)
    GH["hits"] = [{"number": 12, "title": "Dependency Dashboard", "user": "sam"}]
    look(10 * DAY + 2 * D.HOUR)
    assert [a for a in CALLS if "search/issues" in a] and ("fresh", "repos/sam/r/issues/12", None) in CALLS, CALLS
    assert set(C.load("dashboard-sam_r.json", {})["items"]) == set(D.read(BODY)), change
    GH["hits"] = [{"number": 7, "title": "Dependency Dashboard", "user": "sam"}]

# Once Renovate runs as the machine's own App, the user's dashboard from
# before is someone else's: it goes, and the App's is found in its place.
C.save("dashboard-sam_r.json", {})
dashboard(BODY)
look(0)
assert C.load("dashboard-sam_r.json", {}).get("n") == 7, C.load("dashboard-sam_r.json", {})
C.CFG["dashboard"]["renovate"] = "workers[bot]"
assert look(DAY) == [] and not JOBS, JOBS
assert C.load("dashboard-sam_r.json", {}) == {"next": T + DAY + 2 * D.HOUR}, C.load("dashboard-sam_r.json", {})
dashboard(BODY, who="workers[bot]")
GH["hits"] = [{"number": 7, "title": "Dependency Dashboard", "user": "workers[bot]"}]
look(DAY + 2 * D.HOUR)
assert set(C.load("dashboard-sam_r.json", {})["items"]) == set(D.read(BODY)), C.load("dashboard-sam_r.json", {})
C.CFG["dashboard"]["renovate"] = None
GH["hits"] = [{"number": 7, "title": "Dependency Dashboard", "user": "sam"}]

# GitHub not answering keeps all of it, for the next pass to read again.
C.save("dashboard-sam_r.json", {})
dashboard(BODY)
look(0)
was = C.load("dashboard-sam_r.json", {})
ISSUE["raise"] = C.Unavailable("gh api repos/sam/r/issues/7: HTTP 502")
try:
    look(60)
    raise AssertionError("no raise")
except C.Unavailable:
    pass
assert C.load("dashboard-sam_r.json", {}) == was
ISSUE.pop("raise")

# None found, or only an issue by another title: no read, and the search
# waits a day; and a pass searches for so many repos at most.
GH["hits"] = [{"number": 9, "title": "Dependency Dashboard (old)", "user": "sam"}]
C.save("dashboard-sam_r.json", {})
look(0)
assert C.load("dashboard-sam_r.json", {}) == {"n": None, "next": T + DAY} and not [a for a in CALLS if a[0] == "fresh"], CALLS
look(DAY - 1)
assert not CALLS, CALLS
C.NOW = T
D.FOUND.clear()
for i in range(D.FINDS + 3):
    C.save(f"dashboard-sam_r{i}.json", {})
    D.watch(f"sam/r{i}", [])
assert len(D.FOUND) == D.FINDS and C.load(f"dashboard-sam_r{D.FINDS}.json", {}) == {}, D.FOUND
GH["hits"] = [{"number": 7, "title": "Dependency Dashboard", "user": "sam"}]

# No job could start: listed, tried again in five minutes, and nothing is
# taken for handed.
C.save("dashboard-sam_r.json", {})
dashboard(BODY)
look(0)
GH["job"] = None
mine = look(DAY)
assert mine == [(D.NO_JOB, "sam/r#7", "for what's stuck on Renovate's dashboard")] and C.WAKE == {"sam/r": T + DAY + 300}, (mine, C.WAKE)
assert not any("handed" in it for it in C.load("dashboard-sam_r.json", {})["items"].values())
GH.pop("job")

# An error still there after two of Renovate's runs on the repo since it
# showed up is stuck then, short of a day: one run isn't two, a run before it
# doesn't count, and the waits wait their days. The runs over every repo
# count with the repo's own.
C.save("dashboard-sam_r.json", {})
dashboard(BODY)
look(0)
ran = C.CFG["dashboard"]["renovateRuns"]
os.makedirs(ran)
with open(f"{ran}/all", "w") as f:
    f.write(f"{T - 600}\n{T + 600}\n")
assert look(1200) == [] and not JOBS and C.WAKE == {"sam/r": T + DAY}, (JOBS, C.WAKE)
with open(f"{ran}/sam_r", "w") as f:
    f.write(f"{T + 1500}\n")
assert look(1800) == [] and len(JOBS) == 1, JOBS
task = JOBS[0][1]
assert "Repository Problems: WARN: Found renovate config warnings" in task and "couldn't look up `left-pad`" in task, task
assert "clap" not in task and "Config Migration Needed" not in task, task
for f in ("all", "sam_r"):
    os.remove(f"{ran}/{f}")
os.rmdir(ran)

# In the steward's pass: each repo's dashboard is looked at, after its PRs,
# when it's on, and one that fails doesn't stop the rest.
seen, published = [], []
kept_fns = (S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, D.watch)
S.R.settle_repo = lambda r, known: False
S.R.discover = lambda: [{"repo": "sam/r", "default": "main"}, {"repo": "sam/q", "default": "main"}]
S.R.publish = published.append
S.open_prs = lambda owner, name: (seen.append(f"prs {owner}/{name}") or {}, [])


def watch(repo, mine):
    seen.append(f"dashboard {repo}")
    if repo == "sam/r":
        raise RuntimeError("GitHub said no")
    mine.append((D.STUCK, f"{repo}#7", "x"))


D.watch, argv = watch, sys.argv
sys.argv = ["steward", "kick", "sam/r", "sam/q"]
C.NOW = T
C.WAKE.clear()
S.main()
assert seen == ["prs sam/r", "dashboard sam/r", "prs sam/q", "dashboard sam/q"], seen
assert published[-1] == [(D.STUCK, "sam/q#7", "x")] and C.load("wake.json", {})["sam/r"] == T + 300, (published, C.load("wake.json", {}))
C.CFG["dashboard"]["enable"] = False
seen.clear()
S.main()
assert seen == ["prs sam/r", "prs sam/q"], seen
sys.argv = argv
S.R.discover, S.R.settle_repo, S.R.publish, S.open_prs, D.watch = kept_fns

C.gh, C.fresh, C.job, C.job_state = kept[:4]
C.CFG["dashboard"].clear()
C.CFG["dashboard"].update(kept[4])
print("dashboards: ok")
