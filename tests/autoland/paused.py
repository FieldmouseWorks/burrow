from harness import admission, DEFAULT_ADMISSION
"""Paused repositories never reach the steward's cloning or job startup path."""
from unittest.mock import patch

from harness import C, REAL

C.CFG["settings"] = admission(["sam/r", "sam/other"], ["sam/other"], 4102444800)
with patch.object(C, "retiring", side_effect=AssertionError("admitted")):
    for role in ("steward-handed", "steward-judged", "proof"):
        try:
            REAL["job"]("sam/r", "must not start", clone=True, role=role)
        except C.JobPaused:
            pass
        else:
            raise AssertionError("paused job was admitted")
    # A listed pipeline repository still reaches normal admission.
    try:
        REAL["job"]("sam/other", "task", role="steward-handed")
    except AssertionError as error:
        assert str(error) == "admitted"
    else:
        raise AssertionError("pipeline repository was paused")
C.CFG["settings"] = DEFAULT_ADMISSION

# Exercise the PR transaction boundary: paused fix/judge/proof admissions
# become holds with no strikes or failure digest. Settled PRs stay held too.
from harness import JOBS, S, R, CI_GREEN, fresh_pr_state, clean, mkpr, run, pr_state, merged, rv_review, stand_in

stand_in()
C.CFG["settings"] = admission(["sam/r", "sam/other"], ["sam/other"], 4102444800)
with patch.object(C, "job", REAL["job"]):
    saved_repo = dict(C.CFG["repos"].get("sam/r", {}))
    C.CFG["repos"]["sam/r"] = {"proofJobs": {"paused-proof": "run proof"}}
    cases = [
        mkpr(reviews=[rv_review("codex", 1)] * 5, threads=[False]),
        clean(checks=[]),
        clean(checks=CI_GREEN + [{"__typename": "StatusContext", "context": "paused-proof", "state": "PENDING", "description": ""}]),
        clean(),
    ]
    for pr in cases:
        # Keep the merge case free of required agent proofs.
        C.CFG["repos"]["sam/r"] = {} if pr is cases[-1] else {"proofJobs": {"paused-proof": "run proof"}}
        fresh_pr_state()
        # The first pass establishes the head's settle time.
        run(pr, 0)
        status, digest = run(pr, 7200)
        assert "Automated jobs paused" in status, status
        assert digest == [] and JOBS == [], (digest, JOBS)
        assert not pr_state().get("strikes"), pr_state()
        assert not pr_state().get("landed"), (status, pr_state())
    # Listed pipeline work still settles and lands under the same mode.
    C.CFG["settings"] = admission(["sam/r"], ["sam/r"], 4102444800)
    C.CFG["repos"]["sam/r"] = {}
    fresh_pr_state()
    pr = clean()
    run(pr, 0)
    status, _ = run(pr, 7200)
    assert pr_state().get("landed") == pr["headRefOid"], (status, pr_state())
    C.CFG["settings"] = admission(["sam/r", "sam/other"], ["sam/other"], 4102444800)
    mine = []
    out = {"stuck": [], "failed": [], "wake": None, "searched": 0,
           "jobs": [{"task": "fix", "pipeline": True, "timeout": 60, "key": "pause",
                     "role": "flaky", "part": "flaky", "unstarted": [["failure", "line", "why"]]}]}
    with patch.object(C, "upkeep", return_value=out), patch.object(S, "failing", side_effect=AssertionError("failure")):
        S.upkeep("sam/r", mine)
    assert mine == [], mine
    C.save("repo-sam_r.json", {})
    info = {"defaultBranchRef": {"name": "main", "target": {
        "oid": "b" * 40, "statusCheckRollup": {"state": "FAILURE"}}}}
    R.watch_default({"repo": "sam/r", "default": "main"}, info, [], mine)
    assert mine == [] and not C.load("repo-sam_r.json", {}).get("red", {}).get("job"), mine
C.CFG["repos"]["sam/r"] = saved_repo
C.CFG["settings"] = DEFAULT_ADMISSION
stand_in()

# Scope narrows the same allow list, with no cached reader state.
C.CFG["settings"] = admission(["sam/r", "sam/other"], ["sam/other"], 4102444800)
C.WAKE.pop("sam/r", None)
assert C.paused("sam/r")
assert C.WAKE["sam/r"] == 4102444800
assert not C.paused("sam/other")
assert C.paused("sam/new")
C.CFG["settings"] = DEFAULT_ADMISSION
