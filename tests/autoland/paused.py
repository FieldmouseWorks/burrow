"""Paused repositories never reach the steward's cloning or job startup path."""
from unittest.mock import patch

from harness import C, REAL

C.CFG["paused"] = ["SAM/R"]
with patch.object(C, "retiring", side_effect=AssertionError("admitted")):
    for role in ("steward-handed", "steward-judged", "proof"):
        try:
            REAL["job"]("sam/r", "must not start", clone=True, role=role)
        except C.JobPaused:
            pass
        else:
            raise AssertionError("paused job was admitted")
    # An unlisted repository still reaches normal admission.
    try:
        REAL["job"]("sam/other", "task", role="steward-handed")
    except AssertionError as error:
        assert str(error) == "admitted"
    else:
        raise AssertionError("unlisted repository was paused")
C.CFG.pop("paused")

# Exercise the PR transaction boundary: paused fix/judge/proof admissions
# become holds with no strikes or failure digest. Already settled PRs merge.
from harness import JOBS, S, R, CI_GREEN, fresh_pr_state, clean, mkpr, run, pr_state, merged, rv_review, stand_in

stand_in()
C.CFG["paused"] = ["SAM/R"]
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
        # The clean PR can merge without a job; the fix requires one.
        if pr is not cases[-1]:
            assert "Automated jobs paused" in status, status
            assert digest == [] and JOBS == [], (digest, JOBS)
            assert not pr_state().get("strikes"), pr_state()
        else:
            assert pr_state().get("landed") == pr["headRefOid"], (status, pr_state())
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
C.CFG.pop("paused")
stand_in()
