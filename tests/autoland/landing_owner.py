"""Who lands sam/r (landing.py, Conary#313), through the real settings
binary reading a landing record as burrow-landing writes it: where conaryd
holds the repo, or it's on its way there, the steward's pass, kicked or
full, sees to none of its PRs (no step, job, proof, merge, train or branch
deleted) and books nothing for them, and each landing write refuses on its
own, a handoff of the PR included. Letting the repo go, the steward stops
its PRs' jobs and proof builds, and says it has released the repo only once
none is left. Where the steward holds the repo, it lands as before."""
import json
import os
import subprocess
import tempfile
from contextlib import ExitStack
from unittest.mock import patch

from harness import (C, DEFAULT_ADMISSION, JOBS, S, SETTINGS, STOPPED, UNSTOPPABLE, clean, fresh_pr_state,
                     info, merges, pr_state, run, stand_in, tmp)
import landing as L
import proofs as P
import prs
import train as T

stand_in()
SAVED = dict(C.CFG["repos"].get("sam/r", {}))
C.CFG["repos"]["sam/r"] = {}
A = "a" * 40


def landing(owner, to=None, generation=1):
    """sam/r allowed, and its landing record saying this."""
    folder = tempfile.mkdtemp(dir=tmp)
    record = f"{folder}/owners.json"
    with open(record, "w") as f:
        json.dump({"repos": {"sam/r": {"owner": owner, "to": to, "generation": generation, "since": 0}}}, f)
    path = f"{folder}/tracked.json"
    with open(path, "w") as f:
        json.dump({"overlay": f"{folder}/overlay.json", "allowed": ["sam/r"], "pipelineRepos": [],
                   "landingRecord": record, "conaryd": ["sam/r"]}, f)
    return {"command": SETTINGS, "config": path}


def a_pass(kick):
    """One steward pass over sam/r, everything but its landing gate stood
    in: what it reached."""
    reached = []

    def note(name):
        return lambda *a, **k: reached.append(name) or {}

    C.WAKE.clear()
    with ExitStack() as stack:
        for obj, name, fn in (
            (S.R, "discover", lambda: [{"repo": "sam/r", "default": "main"}]), (C, "follow_renames", lambda *a: None),
            (S.R, "restart", lambda *a: None), (S.R, "settle_repo", lambda *a: False),
            (S.R, "watch_default", note("watch_default")), (S.R, "publish", lambda *a: None),
            (S.R, "hooks", lambda *a: None), (S, "open_prs", lambda *a: reached.append("open_prs") or (info, [clean()])),
            (S.P, "begin", lambda *a: None), (S.T, "begin", note("train")), (S.T, "on", lambda *a: True),
            (S.T, "step", note("train")), (S.step, "handle", note("handle")),
            (S, "branches", note("branches")), (S, "upkeep", note("upkeep")),
            (C, "taken", lambda *a: None), (C, "poke", lambda *a: None),
            (C, "coverage", lambda rs: {"repos": {r["repo"]: {} for r in rs}}),
        ):
            stack.enter_context(patch.object(obj, name, fn))
        S.run_pass(kick)
    return reached


def marker():
    try:
        with open(os.path.join(C.CFG["stateDir"], L.MARKER)) as f:
            return json.load(f)
    except FileNotFoundError:
        return {}


# conaryd holds sam/r, or it's on its way there or back: a pass sees to its
# upkeep and nothing of its PRs, and books no wake-up for them.
for holding in (("conaryd", None), ("conaryd", "steward"), ("steward", "conaryd")):
    C.CFG["settings"] = landing(*holding)
    fresh_pr_state()
    for kick in (["sam/r"], None):
        reached = a_pass(kick)
        assert reached == ["upkeep"], (holding, kick, reached)
        assert "sam/r" not in C.WAKE, (holding, kick, C.WAKE)
    # Its PRs land all the same: renovate-sweep is told conaryd lands them,
    # or the one a handover goes to, never nothing.
    lands = C.coverage([{"repo": "sam/r", "default": "main"}])["repos"]["sam/r"]["lands"]
    assert lands == (holding[1] or holding[0]), (holding, lands)
# And one whose record can't be read: nobody knows who lands it.
C.CFG["settings"] = {"command": "false", "config": "/nonexistent"}
assert C.coverage([{"repo": "sam/r", "default": "main"}])["repos"]["sam/r"]["lands"] is None

# And every landing write refuses on its own, whatever reaches it.
C.CFG["settings"] = landing("conaryd")
fresh_pr_state()
with patch.object(C, "job", side_effect=AssertionError("a job started in conaryd's repo")):
    run(clean(sha=A), 0)
    status, _ = run(clean(sha=A), 7200)
assert merges() == [] and JOBS == [], (merges(), JOBS)
with patch.object(C, "write", side_effect=AssertionError("merged in conaryd's repo")), \
        patch.object(C, "unland", return_value=True), patch.object(prs, "merge_method", return_value="squash"):
    try:
        prs.land("sam/r", clean(sha=A), {})
    except C.JobPaused:
        pass
    else:
        raise AssertionError("the steward merged in conaryd's repo")
with patch.object(C, "remote", side_effect=AssertionError("train write in conaryd's repo")):
    for action in ("join", "step"):
        try:
            T.cn("train", action, "sam/r", "5", "--head", A)
        except C.JobPaused:
            pass
        else:
            raise AssertionError(f"train {action} in conaryd's repo")
assert P.start("proof-x", {"repo": "sam/r"}, "a build") is None
# A handoff of the PR at its head is the steward's only where it lands:
# settings refuses one in conaryd's repo, and one given while the steward
# held it lets nothing by once conaryd does.
settings = lambda *a: subprocess.run([SETTINGS, C.CFG["settings"]["config"], *a], capture_output=True)  # noqa: E731
assert settings("land", "sam/r#5", A).returncode != 0
C.CFG["settings"] = landing("steward")
assert settings("pause", "sam/r").returncode == 0 and settings("land", "sam/r#5", A).returncode == 0
assert C.handed("sam/r", {"number": 5, "headRefOid": A})
record = json.load(open(C.CFG["settings"]["config"]))["landingRecord"]
with open(record, "w") as f:
    json.dump({"repos": {"sam/r": {"owner": "conaryd", "to": None, "generation": 2, "since": 0}}}, f)
assert not C.handed("sam/r", {"number": 5, "headRefOid": A})
assert C.held("sam/r", {"number": 5, "headRefOid": A})

# Letting sam/r go to conaryd: the jobs and proof builds its PRs' states
# name, still at work, are stopped, and the release says which handover.
C.CFG["settings"] = landing("steward", "conaryd", generation=3)
fresh_pr_state()
STOPPED.clear()
C.save("pr-sam_r-5.json", {"fixed": "x", "fixedJob": "job-a", "provedJob": "proof-x", "proofsLive": ["job-b"]})
C.save("pr-sam_r-6.json", {"judgedJob": "job-c"})
C.save("pr-sam_rr-7.json", {"fixedJob": "job-other"})  # another repo's
assert not L.lands("sam/r")
assert sorted(STOPPED) == ["job-a", "job-b", "job-c", "proof-x"], STOPPED
assert marker() == {"sam/r": 3}, marker()
# One proj won't stop keeps the repo the steward's: no release, and a look
# again a few minutes on.
C.CFG["settings"] = landing("steward", "conaryd", generation=4)
UNSTOPPABLE.add("job-d")
C.save("pr-sam_r-8.json", {"fixedJob": "job-d"})
C.WAKE.clear()
assert not L.lands("sam/r")
assert marker() == {"sam/r": 3}, marker()
assert C.WAKE.get("sam/r", 0) > C.NOW, C.WAKE
UNSTOPPABLE.discard("job-d")
assert not L.lands("sam/r")
assert marker() == {"sam/r": 4}, marker()

# The steward's own, as the record says or with no record at all: it lands.
for settings in (landing("steward"), DEFAULT_ADMISSION):
    C.CFG["settings"] = settings
    fresh_pr_state()
    reached = a_pass(["sam/r"])
    assert "handle" in reached and "train" in reached and "branches" in reached, reached
    run(clean(sha=A), 0)
    status, _ = run(clean(sha=A), 7200)
    assert pr_state().get("landed") == A, (status, pr_state())

C.CFG["repos"]["sam/r"] = SAVED
C.CFG["settings"] = DEFAULT_ADMISSION
stand_in()
