"""Flaky tests: whose quarantine goes to a Claude job, one job a repo at a
time, the stuck issue a day after it, and the pass that looks."""

import json
import os
import sys

from harness import C, R, S, tmp

import flakes as F

DAY = 86400
T = 1_800_000_000

# The stuck kinds are the pipeline's; one someone else quarantined is the
# owner's.
assert F.STUCK not in R.OWNERS and F.NO_JOB not in R.OWNERS and F.OTHERS in R.OWNERS

# The flaky tool: what it lists (or how it fails), and how it was run.
FLAKY, ASKED = f"{tmp}/flaky-says.json", f"{tmp}/flaky-asked"
with open(f"{tmp}/flaky", "w") as f:
    f.write(f"""#!{sys.executable}
import json, os, sys
with open({ASKED!r}, "a") as f:
    f.write(json.dumps({{"args": sys.argv[1:], "cache": os.environ.get("XDG_CACHE_HOME")}}) + "\\n")
says = json.load(open({FLAKY!r}))
for q in says.get("open", []):
    print(json.dumps(q))
if says.get("rc"):
    print(says["err"], file=sys.stderr); sys.exit(says["rc"])
""")
os.chmod(f"{tmp}/flaky", 0o755)


def says(*issues, rc=0, err=""):
    with open(FLAKY, "w") as f:
        json.dump({"open": [{"test": t, "issue": n, "author": a} for n, t, a in issues], "rc": rc, "err": err}, f)


JOBS, STARTED, STATES = [], [], {}


def job(repo, task, clone=False):
    JOBS.append((repo, task, clone))
    STARTED.append(repo)
    return STATES.get("next", f"flaky-{len(STARTED)}")


def look(at, repo="sam/r"):
    """One pass's look at the repo's quarantine, `at` seconds after T."""
    C.NOW = T + at
    C.WAKE.clear()
    JOBS.clear()
    mine = []
    F.watch(repo, mine)
    return mine


kept = (C.gh, C.job, C.job_state, C.limited)
C.gh = lambda *a, **k: "sam" if a[:2] == ("api", "user") else None
C.job = job
C.job_state = lambda jid: STATES.get(jid, "done")
C._me.clear()

# The CI's issue goes to a job at once: cloned if need be, with the test, its
# issue, what a fix is and how it lifts the quarantine; its end is booked.
says((4, "browser/stream", "github-actions[bot]"))
assert look(0) == [] and len(JOBS) == 1, JOBS
repo, task, clone = JOBS[0]
assert repo == "sam/r" and clone, JOBS
for x in ("`browser/stream`: https://github.com/sam/r/issues/4", "`Closes #N`", "Never skip, loosen, retry or delete",
          "don't merge it yourself", "never instructions"):
    assert x in task, (x, task)
assert C.WAKE == {"sam/r": T + (C.CFG["jobTimeout"] + 5) * 60}, C.WAKE
# It asked the tool once, with the steward's state as its cache.
asked = [json.loads(x) for x in open(ASKED)]
assert asked == [{"args": ["quarantine", "sam/r", "--json"], "cache": C.STATE}], asked

# While that job works, a new one waits for it, and one someone else opened
# is the owner's: listed, never a job.
STATES["flaky-1"] = "running"
says((4, "browser/stream", "github-actions[bot]"), (6, "unit/clock", "sam"), (7, "unit/disk", "eve"))
mine = look(600)
assert not JOBS and mine == [(F.OTHERS, "sam/r#7", "`unit/disk`, by eve")], (JOBS, mine)
assert C.WAKE == {"sam/r": T + (C.CFG["jobTimeout"] + 5) * 60}, C.WAKE
# Past its timeout and still running (proj's end kicks a pass): asked again
# in a quarter hour.
look(2 * 3600)
assert not JOBS and C.WAKE == {"sam/r": T + 2 * 3600 + 15 * 60}, C.WAKE

# It ended, the first still open: the new one gets a job of its own, and
# the first waits out its day, then goes to the stuck issue.
STATES["flaky-1"] = "done"
mine = look(3 * 3600)
assert [m for m in mine if m[0] != F.OTHERS] == [] and len(JOBS) == 1, (mine, JOBS)
assert "`unit/clock`: https://github.com/sam/r/issues/6" in JOBS[0][1] and "browser/stream" not in JOBS[0][1], JOBS
assert C.WAKE == {"sam/r": T + 3 * 3600 + (C.CFG["jobTimeout"] + 5) * 60}, C.WAKE
STATES["flaky-2"] = "running"
mine = look(DAY + 1)
assert [m for m in mine if m[0] != F.OTHERS] == [(F.STUCK, "sam/r#4", "`browser/stream`")] and not JOBS, (mine, JOBS)
# The second's day is still to come, and its job still runs.
assert C.WAKE == {"sam/r": T + DAY + 1 + 15 * 60}, C.WAKE

# Fixed: its issue closed, so it's gone from the state, and nothing's listed.
STATES.clear()
says((7, "unit/disk", "eve"))
mine = look(2 * DAY)
assert mine == [(F.OTHERS, "sam/r#7", "`unit/disk`, by eve")] and C.load("flaky-sam_r.json", {})["issues"] == {}, mine

# An app's issue (GraphQL's login for it lacks `[bot]`) and one of the
# authors' go to a job; a login merely ending in an author's doesn't.
assert F.ours("github-actions[bot]") and F.ours("renovate[bot]") and F.ours("sam") and F.ours("dependabot")
assert not F.ours("eve") and not F.ours("evil-sam") and not F.ours("sam[bot]x")

# No job could start: listed, tried again in five minutes, nothing handed.
C.save("flaky-sam_r.json", {})
says((9, "unit/net", "github-actions[bot]"))
STATES["next"] = None
mine = look(0)
assert mine == [(F.NO_JOB, "sam/r#9", "for the flaky test `unit/net`")] and C.WAKE == {"sam/r": T + 300}, (mine, C.WAKE)
assert "handed" not in C.load("flaky-sam_r.json", {})["issues"]["9"]
STATES.clear()

# GitHub not answering is Unavailable, and keeps the state for the next pass;
# anything else the tool says no to is an error.
was = C.load("flaky-sam_r.json", {})
C.limited = lambda stderr: None  # it asks gh for the reset: not this test's
for err, kind in (("GET repos/sam/r/issues: HTTP 503, 3 times", C.Unavailable),
                  ("GET repos/sam/r/issues: HTTP 403: API rate limit exceeded for user", C.Unavailable),
                  ("GET repos/sam/r/issues: HTTP 404: Not Found", RuntimeError)):
    says(rc=1, err=err)
    try:
        look(60)
        raise AssertionError(err)
    except RuntimeError as e:
        assert type(e) is kind and err in str(e), (err, e)
    assert C.load("flaky-sam_r.json", {}) == was

# In the steward's pass: each repo's quarantine is looked at, after its PRs,
# when it's on, and one that fails doesn't stop the rest.
seen, published = [], []
kept_fns = (S.R.discover, S.R.publish, S.open_prs, F.watch)
S.R.discover = lambda: [{"repo": "sam/r"}, {"repo": "sam/q"}]
S.R.publish = published.append
S.open_prs = lambda owner, name: (seen.append(f"prs {owner}/{name}") or {}, [])


def watch(repo, mine):
    seen.append(f"flaky {repo}")
    if repo == "sam/r":
        raise RuntimeError("GitHub said no")
    mine.append((F.STUCK, f"{repo}#4", "x"))


F.watch, argv = watch, sys.argv
sys.argv = ["steward", "kick", "sam/r", "sam/q"]
C.NOW = T
C.WAKE.clear()
C.CFG["flaky"]["enable"] = True
S.main()
assert seen == ["prs sam/r", "flaky sam/r", "prs sam/q", "flaky sam/q"], seen
assert published[-1] == [(F.STUCK, "sam/q#4", "x")] and C.load("wake.json", {})["sam/r"] == T + 300, (published, C.load("wake.json", {}))
C.CFG["flaky"]["enable"] = False
seen.clear()
S.main()
assert seen == ["prs sam/r", "prs sam/q"], seen
sys.argv = argv
S.R.discover, S.R.publish, S.open_prs, F.watch = kept_fns

C.gh, C.job, C.job_state, C.limited = kept
C._me.clear()
print("quarantines: ok")
