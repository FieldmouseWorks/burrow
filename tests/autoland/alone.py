"""A red default branch one landing alone turned red: its tip one merged
PR's landing, its first parent green, a check run on it failed. The steward
names that PR itself and has the ledger's writer cn undo it, with no Claude
job (repos.landed_alone). Anything less certain (a parent pending or with
no CI, two landings since green, a red no check run failed for, a tip
that's an undo or a red fix itself) starts the job as ever; so does an undo
the ledger refused, or one that left the branch red, and the job is told
which."""

import os

from harness import C, FAIL, JOBS, R, STATES, WROTE, mkpr, tmp
from undos import ROW, undo_pr, writer, writer_runs

C._for.append("sam")
kept_undo = C.CFG.get("undo")
C.CFG["undo"] = f"{tmp}/writer-cn"

repo = {"repo": "sam/u", "private": False, "default": "main"}
mine = []
TIP = "c" * 40
# A retry's time is the clock's (repos.clock), here the pass's.
kept_now, kept_clock = C.NOW, R.clock
R.clock = lambda: C.NOW


def landed(number=12, sha=TIP, head="feature", body="It does a thing.", base="main", state="MERGED"):
    return {"number": number, "state": state, "headRefName": head, "baseRefName": base, "body": body,
            "mergeCommit": {"oid": sha}}


def default(state="FAILURE", sha=TIP, parent="SUCCESS", runs=("SUCCESS", "FAILURE"), prs=None):
    """main at sha, its CI at state, one PR's landing (prs: what GitHub ties
    to the commit), its first parent's CI at parent (None: no CI on it),
    and its check runs ending as runs say."""
    return {"defaultBranchRef": {"name": "main", "target": {
        "oid": sha,
        "statusCheckRollup": {"state": state, "contexts": {"nodes": [{"conclusion": c} for c in runs]}},
        "parents": {"nodes": [{"statusCheckRollup": {"state": parent} if parent else None}]},
        "associatedPullRequests": {"nodes": [landed()] if prs is None else prs}}}}


def red():
    return C.load("repo-sam_u.json", {}).get("red") or {}


def fresh():
    C.save("repo-sam_u.json", {})
    JOBS.clear()
    C.WAKE.clear()
    for j in ("job-1", "job-2"):
        STATES.pop(j, None)
    writer(out=[ROW])


# One landing turned it red: undone at once, as the writer, with no job.
fresh()
assert R.watch_default(repo, default(), [], mine) == {}
assert writer_runs() == [{"args": ["undo", "sam/u#12"], "tokens": []}], writer_runs()
assert JOBS == [] and mine == [], (JOBS, mine)
assert red()["undos"] == [40] and red()["asked"] and red()["undone"] == ["sam/u#12"], red()
assert C.WAKE.get("sam/u") == C.NOW + 60, C.WAKE
# Its undo is the red's fix, as a job's would be: held while main's CI runs,
# and nothing more asked or started while it's open.
writer()
for _ in range(2):
    assert R.watch_default(repo, default(), [undo_pr()], mine) == {}
assert R.watch_default(repo, default("PENDING"), [undo_pr()], mine) == {
    40: "Waiting for CI on main: it lands only while main is red"}
assert writer_runs() == [] and JOBS == [], (writer_runs(), JOBS)
# Green again without it: it's closed, and the red forgotten.
WROTE.clear()
assert R.watch_default(repo, default("SUCCESS", sha="e" * 40), [undo_pr()], mine) == {
    40: "Not needed: main is green again without it"}
assert [x.split()[:3] for x in WROTE] == [["pr", "close", "40"]] and not red(), (WROTE, red())

# Green before its PR shows in what a pass read: one more look first, so
# it's closed, not left to land on a green main.
fresh()
R.watch_default(repo, default(), [], mine)
assert R.watch_default(repo, default("SUCCESS", sha="e" * 40), [], mine) == {} and red(), red()
WROTE.clear()
assert R.watch_default(repo, default("SUCCESS", sha="e" * 40), [undo_pr()], mine) == {
    40: "Not needed: main is green again without it"}
assert [x.split()[:3] for x in WROTE] == [["pr", "close", "40"]] and not red(), (WROTE, red())

# Landed, and main is red still: a job starts (after the second look), told
# what was undone and not to name it again; if it does anyway, nothing is
# asked of cn.
fresh()
R.watch_default(repo, default(), [], mine)
writer(out=[ROW])
after = default(sha="d" * 40, parent="FAILURE", prs=[landed(40, sha="d" * 40, head="conary/undo/c12/1")])
R.watch_default(repo, after, [], mine)
assert JOBS == [], JOBS
R.watch_default(repo, after, [], mine)
assert len(JOBS) == 1 and "undo sam/u#12 for this red already" in JOBS[0] and "Don't name it again" in JOBS[0], JOBS
assert '{"undo": "sam/u#N"}' in JOBS[0] and red()["undone"] == ["sam/u#12"] and red()["jobs"] == 1, (JOBS, red())
STATES["job-1"] = "done"
os.makedirs(f"{tmp}/jobs/job-1", exist_ok=True)
with open(f"{tmp}/jobs/job-1/answers.json", "w") as f:
    f.write('{"undo": "sam/u#12"}')
R.watch_default(repo, after, [], mine)
R.watch_default(repo, after, [], mine)
assert writer_runs() == [] and len(JOBS) == 2, (writer_runs(), JOBS)
assert "undo sam/u#12 for this red already" in JOBS[1], JOBS[1]

# The ledger refuses it: a job starts, told what cn said, and not to name one.
fresh()
writer(rc=65, err="cn: no landed change for sam/u#12")
R.watch_default(repo, default(), [], mine)
assert len(writer_runs()) == 1 and red()["refused"] and JOBS == [], (writer_runs(), red(), JOBS)
R.watch_default(repo, default(), [], mine)
assert len(JOBS) == 1 and "no landed change for sam/u#12" in JOBS[0] and "Don't name one again" in JOBS[0], JOBS
assert '{"undo"' not in JOBS[0] and red()["refused"] and red()["jobs"] == 1, (JOBS, red())

# cn stopping partway is tried again, as for a job's undo, with no job yet.
fresh()
writer(rc=75, err="cn: GitHub is unavailable")
R.watch_default(repo, default(), [], mine)
assert red()["undoTries"] == 1 and not red().get("asked") and JOBS == [], (red(), JOBS)

# A red fix opened before that retry: it's the fix in hand, so cn isn't
# asked again, and no job starts, while it's open. Without it, the retry
# runs as booked.
C.NOW = red()["retryAt"]
fix = mkpr(branch="autoland/red-bbbbbbbbbbbb")
for _ in range(2):
    assert R.watch_default(repo, default(), [fix], mine) == {}
assert len(writer_runs()) == 1 and red()["undoTries"] == 1 and JOBS == [], (writer_runs(), red(), JOBS)
writer(out=[ROW])
R.watch_default(repo, default(), [], mine)
assert len(writer_runs()) == 1 and red()["undos"] == [40] and red()["asked"], (writer_runs(), red())
C.NOW = kept_now
# One cn opened before it stopped is no other fix: the retry records it,
# whatever red fix is open beside it.
fresh()
writer(rc=1, out=[{"unrecorded": "https://github.com/sam/u/pull/40", "pr": 40}], err="cn: GitHub said 502")
R.watch_default(repo, default(), [], mine)
assert red()["undos"] == [40] and not red().get("asked"), red()
C.NOW = red()["retryAt"]
writer(out=[dict(ROW, opened=False)])
assert R.watch_default(repo, default(), [undo_pr(), fix], mine) == {}
assert len(writer_runs()) == 1 and red()["undos"] == [40] and red()["asked"] and JOBS == [], (writer_runs(), red(), JOBS)
C.NOW = kept_now

# Anything less certain starts the job as ever, and asks cn nothing.
for why, info in (
        ("its parent's CI is pending", default(parent="PENDING")),
        ("its parent has no CI (a train's member landed after another)", default(parent=None)),
        ("its parent's CI was cancelled", default(parent="ERROR")),
        ("its parent is red too: two landings since green", default(parent="FAILURE")),
        ("no parent read", dict(default(), defaultBranchRef={"name": "main", "target": dict(
            default()["defaultBranchRef"]["target"], parents={"nodes": []})})),
        ("only cancelled, timed out or unstarted runs", default(runs=("SUCCESS", "CANCELLED", "TIMED_OUT", "STARTUP_FAILURE"))),
        ("no check runs", default(runs=())),
        ("no PR tied to it (a push)", default(prs=[])),
        ("two PRs tied to it", default(prs=[landed(12), landed(13)])),
        ("a PR that isn't the one it landed as", default(prs=[landed(sha="b" * 40)])),
        ("a PR to another branch", default(prs=[landed(base="dev")])),
        ("an open PR only", default(prs=[landed(state="OPEN")])),
        ("an undo's landing", default(prs=[landed(head="conary/undo/c9/1")])),
        ("an undo's landing, by its marker", default(prs=[landed(
            body='Reverts #9.\n\n<!-- conary-undo: {"reverts":["c9"],"asked":"door"} -->')])),
        ("a red fix's landing", default(prs=[landed(head="autoland/red-bbbbbbbbbbbb")]))):
    fresh()
    R.watch_default(repo, info, [], mine)
    assert writer_runs() == [] and len(JOBS) == 1 and red().get("job") == "job-1", (why, writer_runs(), JOBS, red())
    assert '{"undo": "sam/u#N"}' in JOBS[0] and "for this red already" not in JOBS[0], (why, JOBS[0])

# A red fix open already, with no red seen yet (one for another commit, say):
# it's the fix in hand, so nothing is undone and no job starts. A draft's, a
# fork's, one to another branch or by anyone else isn't, as ever.
for pr in (mkpr(branch="autoland/red-cccccccccccc"), mkpr(branch="autoland/red-bbbbbbbbbbbb")):
    fresh()
    assert R.watch_default(repo, default(), [pr], mine) == {}
    assert writer_runs() == [] and JOBS == [] and not red(), (pr["headRefName"], writer_runs(), JOBS, red())
outside = dict(mkpr(branch="autoland/red-bbbbbbbbbbbb", number=8), author={"__typename": "User", "login": "eve"})
for pr in (mkpr(branch="autoland/red-bbbbbbbbbbbb", draft=True), dict(mkpr(branch="autoland/red-bbbbbbbbbbbb"), isCrossRepository=True),
           mkpr(branch="autoland/red-bbbbbbbbbbbb", base="dev"), outside):
    fresh()
    R.watch_default(repo, default(), [pr], mine)
    assert len(writer_runs()) == 1 and JOBS == [], (pr, writer_runs(), JOBS)

# A red already being seen to isn't looked at again: its job works.
fresh()
writer(rc=65)
R.watch_default(repo, default(parent=None), [], mine)
R.watch_default(repo, default(), [], mine)
assert writer_runs() == [] and len(JOBS) == 1, (writer_runs(), JOBS)

# With the ledger off, there's nothing to undo with: the job starts.
C.CFG["undo"] = None
fresh()
R.watch_default(repo, default(), [], mine)
assert len(JOBS) == 1 and writer_runs() == [], (JOBS, writer_runs())

FAIL.clear()
for j in ("job-1", "job-2"):
    STATES.pop(j, None)
C.CFG["undo"] = kept_undo
R.clock = kept_clock
C.save("repo-sam_u.json", {})
JOBS.clear()
C._for.pop()

print("alone: ok")
