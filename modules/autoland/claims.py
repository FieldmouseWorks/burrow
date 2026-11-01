"""What a Claude job that settles a PR (tasks.fix_task) says it did, and
when that goes up. The job pushes, and leaves its answers (a reply on each
review thread, the PR's new description, a comment) in its answers file
rather than posting them. The PR's state keeps each such job as it was
handed the PR, and each pass hands them to burrow-upkeep `claims`
(crates/burrow-upkeep/src/claims.rs), which decides what becomes of each:
its answers posted once it's over, only while the PR's head is its own
push, or the head it was handed when it pushed nothing (so a job can't say
it fixed something in a commit GitHub doesn't have, as on LoK-web#695); or
the job named to stop, when someone else has moved the PR past it. This
keeps what it gives back in the PR's state and carries out what it says,
through the pass's own steps."""

import json
import os

import common as C
from common import CFG, DRY, log

# The steward's fix jobs, by the kind its state keeps each under (step.ON).
KINDS = ("handed", "redFixed", "failFixed", "conflictFixed")


def answers_file(jid):
    """Where job jid leaves its answers: its own folder of proj's record,
    which outlasts it (its scratch doesn't)."""
    return os.path.join(CFG["jobsDir"], jid, "answers.json")


def brief_file():
    """The same, as the job's brief names it: by its own id, $PROJ_JOB."""
    return answers_file("$PROJ_JOB")


def handed(st, kind, jid, pr):
    """Fix job jid has the PR as it is now: kept until it's over and what
    it left is dealt with. Its brief told it why the last one's answers
    didn't go up (unposted), so that's said."""
    st.setdefault("answering", {})[jid] = {"kind": kind, "head": pr["headRefOid"], "body": pr["body"] or "",
                                           "at": C.NOW}
    st.pop(kind + "Unposted", None)


def look(repo, pr, st, tag, strike, cleared, after):
    """The PR's fix jobs, seen to by `claims`. The PR's status when the
    pass is to stop here, or None: answers went up (what it read of the PR
    is stale; a pass is booked for now), or GitHub turned them away (tried
    again on a later pass, a strike each time, and nothing else is handed
    out meanwhile). A job it names to stop that proj won't is left to
    stopping.py, as a new head's jobs are. Why a job's answers didn't go up
    for want of a push or of answers is kept by its kind (KIND +
    "Unposted"), for its next job's brief."""
    if DRY or not st.get("answering"):
        return None
    out = C.upkeep("claims", repo, input=json.dumps({
        "number": pr["number"], "head": pr["headRefOid"], "branch": pr["headRefName"],
        "machinery": CFG.get("machineryLogin"), "jobs": CFG["jobsDir"], "answering": st["answering"],
        "pushedBy": st.get("pushedBy")}))
    st["answering"], st["pushedBy"] = out["answering"], out["pushedBy"]
    if out["wake"]:
        C.wake(repo, out["wake"])  # ends: claims.rs's next look for who pushed the head, LOOKS at most
    for s in out["stop"]:
        if C.stop_job(s["jid"]):
            log(f"{tag}: stopped Claude job {s['jid']}: {s['why']}")
        else:
            log(f"{tag}: couldn't stop Claude job {s['jid']}, though {s['why']}")
            st["stopping"] = list(dict.fromkeys(st.get("stopping", []) + [s["jid"]]))
            strike("stop")
    status = None
    for a in out["answered"]:
        jid, outcome, why = a["jid"], a["outcome"], a["why"]
        if outcome in ("refused", "unanswered"):
            strike("answer", why)
            status = "GitHub didn't take a Claude job's answers: they go up next try"
            continue
        cleared("answer")
        if outcome == "posted":
            log(f"{tag}: Claude job {jid}'s answers posted")
            after(C.NOW, 0)  # ends: the pass that reads what went up
            status = status or "A Claude job's answers went up: reading the PR again"
        elif outcome == "moved":
            log(f"{tag}: Claude job {jid} stood down, nothing posted: {why}")
        else:
            # It ended without its answer, as one that left none: the PR's
            # still to settle, so this head's next job of the kind (lost)
            # is told why, and a second such fails the PR for the owner.
            log(f"{tag}: Claude job {jid}'s answers not posted: {why}")
            st[a["kind"] + "Unposted"] = {"at": pr["headRefOid"], "why": why}
    return status


def unposted(st, kind, at):
    """Why the last job of the kind on head `at` left answers that didn't go
    up, or None: for the next job of the kind's brief."""
    got = st.get(kind + "Unposted")
    return got["why"] if got and got["at"] == at else None
