"""A PR handed to the steward in a paused repo (`settings land
OWNER/REPO#N HEAD`, admission.py): through the real settings binary and the
steward's own pass, it lands at that head on every normal check, once, and
nothing else in the repo moves. Whatever it still needs (a review, a fix,
a proof, a judge, CI) keeps it waiting, as the pause would."""
import json
import subprocess
from unittest.mock import patch

from harness import (C, CI_GREEN, DEFAULT_ADMISSION, JOBS, REAL, SETTINGS, admission, clean, fresh_pr_state,
                     judgment, merges, mkpr, pr_state, run, said, stand_in)
import prs
import train as T

stand_in()
SAVED = dict(C.CFG["repos"].get("sam/r", {}))
C.CFG["repos"]["sam/r"] = {}
# sam/r is unlisted, so paused; sam/other runs.
C.CFG["settings"] = admission(["sam/other"])
CONFIG = C.CFG["settings"]["config"]


def settings(*args, check=True):
    return subprocess.run([SETTINGS, CONFIG, *args], capture_output=True, text=True, check=check)


def hand(n, sha):
    settings("land", f"sam/r#{n}", sha)


def handed():
    out = json.loads(settings("decision", "sam/r").stdout)
    assert not out["allowed"], out
    return [(h["pr"], h["head"]) for h in out["handed"]]


def two_passes(pr):
    """A fresh PR's first look, then one two hours on: its merges, and the
    later pass's status."""
    fresh_pr_state()
    run(pr, 0)
    first = merges()
    status, digest = run(pr, 7200)
    return status, first + merges()


A, B = "a" * 40, "b" * 40

with patch.object(C, "job", REAL["job"]):
    # Without a handoff, a fully checked PR stays paused, as before.
    status, landed = two_passes(clean(sha=A))
    assert "Automated jobs paused" in status and not landed, (status, landed)

    # Handed at its head: it lands on its clean review, resolved threads and
    # green CI, and the handoff is spent. The repo itself stays paused.
    hand(5, A)
    status, landed = two_passes(clean(sha=A))
    assert len(landed) == 1 and f"sha={A}" in landed[0], (status, landed)
    assert handed() == [], handed()
    assert C.paused("sam/r") and JOBS == [], JOBS

    # Spent: a duplicate event, or a steward that lost its own state
    # (restarted), finds no exception left to use.
    status, landed = two_passes(clean(sha=A))
    assert "Automated jobs paused" in status and not landed, (status, landed)

    # Another PR in the same repo, fully checked too, stays paused while
    # #5 is handed; the handoff of #5 at A admits #5 at A alone.
    hand(5, A)
    status, landed = two_passes(clean(sha=A, number=6))
    assert "Automated jobs paused" in status and not landed, (status, landed)
    assert C.paused("sam/r", {"number": 6, "headRefOid": A})
    assert not C.paused("sam/r", {"number": 5, "headRefOid": A})

    # A head pushed since the handoff isn't the one it was given for.
    status, landed = two_passes(clean(sha=B))
    assert "Automated jobs paused" in status and not landed, (status, landed)
    assert handed() == [(5, A)], handed()

    # Each landing requirement still holds, and what it would start for one
    # (a fix, a judge, a proof, a review) is paused: it waits, untouched.
    PROOF = {"proofJobs": {"proof": "run proof"}}
    for why, pr, rc in (
        ("an open thread", clean(sha=A, threads=[False]), {}),
        ("no review", mkpr(sha=A), {}),
        ("red CI", clean(sha=A, checks=[{**CI_GREEN[0], "conclusion": "FAILURE"}]), {}),
        ("CI still running", clean(sha=A, checks=[{**CI_GREEN[0], "status": "IN_PROGRESS", "conclusion": None}]), {}),
        ("no CI at all", clean(sha=A, checks=[]), {}),
        ("a proof not run", clean(sha=A, checks=CI_GREEN + [
            {"__typename": "StatusContext", "context": "proof", "state": "PENDING", "description": ""}]), PROOF),
        ("a hold", clean(sha=A, labels=("hold",)), {}),
        ("the owner's call", clean(sha=A, labels=("needs-owner",)), {}),
        ("no judgment on an R2 change", clean(sha=A, files=("secrets/remi.yaml",)), {}),
    ):
        C.CFG["repos"]["sam/r"] = rc
        status, landed = two_passes(pr)
        assert not landed and JOBS == [], (why, status, landed, JOBS)
        assert not pr_state().get("strikes"), (why, pr_state())
        assert handed() == [(5, A)], (why, handed())
        print(f"handoff waits on {why}: {status[status.find('description='):][:110]}")
    C.CFG["repos"]["sam/r"] = {}

    # A judged R2 change lands on the judge's verdict, as it would anywhere.
    status, landed = two_passes(clean(sha=A, files=("secrets/remi.yaml",), comments=[said(judgment("land", n=5, sha=A))]))
    assert len(landed) == 1, (status, landed)
    assert handed() == [], handed()

    # A pause written after the handoff revokes it.
    hand(5, A)
    settings("pause", "sam/r")
    status, landed = two_passes(clean(sha=A))
    assert "Automated jobs paused" in status and not landed, (status, landed)
    assert handed() == []

# A pause arriving after the pass's first look stops the merge itself.
hand(5, A)


def revoke(*args):
    settings("pause", "sam/r")
    return "squash"


with patch.object(C, "unland", return_value=True), patch.object(prs, "merge_method", side_effect=revoke), \
        patch.object(C, "write", side_effect=AssertionError("merge escaped the newer pause")):
    try:
        prs.land("sam/r", clean(sha=A), {})
    except C.JobPaused:
        pass
    else:
        raise AssertionError("a revoked handoff merged")

# The train: a handed PR may join at its own head, and the paused repo's
# train steps only while every PR in it is handed, so a handoff never lands
# another PR with it.
hand(5, A)
assert T.admitted("join", "sam/r", "5", "--head", A)
assert not T.admitted("join", "sam/r", "5", "--head", B)
assert not T.admitted("join", "sam/r", "6", "--head", A)
T.TRAINS["sam/r"] = {"joined": [{"pr": 5}], "in_flight": None, "candidates": []}
assert T.admitted("step", "sam/r")
T.TRAINS["sam/r"]["joined"].append({"pr": 6})
assert not T.admitted("step", "sam/r")
T.TRAINS["sam/r"] = {"joined": [], "in_flight": None, "candidates": []}
assert not T.admitted("step", "sam/r")
T.TRAINS.pop("sam/r")

# Handoffs whose PR is no longer open (the train landed it, or it closed)
# are spent on the repo's next pass; one handed after its PRs were read
# isn't, since its PR may have opened since.
hand(9, B)
at = {h["pr"]: h["at"] for h in json.loads(settings("decision", "sam/r").stdout)["handed"]}
C.spend_closed("sam/r", {5, 9}, at[9] + 1)
assert sorted(handed()) == [(5, A), (9, B)], handed()
C.spend_closed("sam/r", {5}, at[9])
assert sorted(handed()) == [(5, A), (9, B)], handed()
C.spend_closed("sam/r", {5}, at[9] + 1)
assert handed() == [(5, A)], handed()
settings("pause", "sam/r")

C.CFG["settings"] = DEFAULT_ADMISSION
C.CFG["repos"]["sam/r"] = SAVED
stand_in()
print("handoff: ok")
