"""What a Claude job that settles a PR (tasks.fix_task) says it did, and
when that goes up. The job pushes, and leaves its answers (a reply on each
review thread, the PR's new description, a comment) in its answers file
rather than posting them: once it's over, the pass hands them to
burrow-upkeep `answer` (crates/burrow-upkeep/src/answer.rs), which posts
them only while the PR's head is the job's own push, or the head it was
handed when it pushed nothing. So a job can't say it fixed something in a
commit GitHub doesn't have (LoK-web#695: its push was refused, and it went
on to reply anyway).

A job whose PR someone else moved on stands down. One still waiting for its
slot when the head moves is stopped: it can't have pushed. One at work is
stopped once GitHub's record of the branch says the head is someone else's
push, not the machinery App's its jobs push as (`pusher`), since whatever
it does next is for a head the PR no longer has; and the head's own fix
job can start rather than wait for it."""

import json
import os

import common as C
from common import CFG, DRY, log

# The steward's fix jobs, by the kind its state keeps each under (step.ON).
KINDS = ("handed", "redFixed", "failFixed", "conflictFixed")
# Each looked for the push behind a head this many times at most, a few
# minutes apart, while GitHub's record of the branch doesn't have it yet.
LOOKS = 3


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
    """Each fix job the PR handed out that's over: what it left, posted, or
    why not. One still at work on a head someone else moved past is
    stopped (one proj won't stop is left to stopping.py, as a new head's
    jobs are). The PR's status when the pass is to stop here, or None:
    answers went up (what it read of the PR is stale; a pass is booked for
    now), or GitHub turned them away (tried again on a later pass, a strike
    each time, and nothing else is handed out meanwhile). Why a job's
    answers didn't go up for want of a push or of answers (its push never
    landed, or they weren't answers) is kept by its kind (KIND +
    "Unposted"), for its next job's brief."""
    if DRY:
        return None
    jobs = st.get("answering", {})
    stop_here = None
    for jid, rec in list(jobs.items()):
        state = C.job_state(jid)
        if state in (None, "queued", "running"):
            if rec["head"] != pr["headRefOid"] and stale(repo, pr, st, jid, state, rec, after):
                why = stale_why(pr, state, st)
                if C.stop_job(jid):
                    log(f"{tag}: stopped Claude job {jid}: {why}")
                else:
                    # Kept for stopping.stop to try again each pass, a
                    # strike each time it can't, as a new head's jobs are.
                    log(f"{tag}: couldn't stop Claude job {jid}, though {why}")
                    st["stopping"] = list(dict.fromkeys(st.get("stopping", []) + [jid]))
                    strike("stop")
                jobs.pop(jid)
            continue
        try:
            with open(answers_file(jid)) as f:
                text = f.read()
        except FileNotFoundError:
            jobs.pop(jid)  # it left nothing to post
            continue
        out = C.upkeep("answer", repo, input=json.dumps({
            "number": pr["number"], "head": rec["head"], "description": rec["body"], "at": rec["at"],
            "answers": text}))
        outcome, why = out["outcome"], out["why"]
        if outcome in ("refused", "unanswered"):
            strike("answer", why)
            stop_here = "GitHub didn't take a Claude job's answers: they go up next try"
            continue
        cleared("answer")
        jobs.pop(jid)
        if outcome == "posted":
            log(f"{tag}: Claude job {jid}'s answers posted")
            after(C.NOW, 0)  # ends: the pass that reads what went up
            stop_here = stop_here or "A Claude job's answers went up: reading the PR again"
        elif outcome == "moved":
            log(f"{tag}: Claude job {jid} stood down, nothing posted: {why}")
        elif outcome in ("wrong", "unpushed"):
            # It ended without its answer, as one that left none: the PR's
            # still to settle, so this head's next job of the kind (lost)
            # is told why, and a second such fails the PR for the owner.
            log(f"{tag}: Claude job {jid}'s answers not posted: {why}")
            st[rec["kind"] + "Unposted"] = {"at": pr["headRefOid"], "why": why}
    return stop_here


def stale(repo, pr, st, jid, state, rec, after):
    """Whether job jid, at work on the head it was handed, which the PR no
    longer has, is to stop: it's still waiting for its slot, or GitHub says
    someone other than the machinery App pushed the PR's head. Without that
    App its jobs push as the user, as the PR's owner may, so a push can't
    be told apart: it carries on, and what it leaves stands down then."""
    if state == "queued":
        return True
    bot = CFG.get("machineryLogin")
    if not bot:
        return False
    by = pushed_by(repo, pr, st, rec, after)
    return by is not None and by != bot


def pushed_by(repo, pr, st, rec, after):
    """Who pushed the PR's head, as GitHub's record of the branch says (kept
    for the head), or None while it can't say: GitHub's record lags a
    push a little, so it's looked for again a few minutes on, LOOKS times."""
    head = pr["headRefOid"]
    known = st.get("pushedBy")
    if known and known[0] == head:
        return known[1]
    out = C.upkeep("pusher", repo, input=json.dumps({"branch": pr["headRefName"], "head": head}))
    if out["by"]:
        st["pushedBy"] = [head, out["by"]]
        return out["by"]
    looked = rec.setdefault("looked", {})
    looked[head] = looked.get(head, 0) + 1
    if looked[head] < LOOKS:
        after(C.NOW, 2)  # ends: the next look for who pushed it, LOOKS at most
    log(f"{repo}#{pr['number']}: can't tell who pushed {head[:7]}: {out['why'] or 'GitHub has no push that left it there'}")
    return None


def stale_why(pr, state, st):
    head = pr["headRefOid"][:7]
    if state == "queued":
        return f"it was queued for a head the PR no longer has (it's at {head} now)"
    return f"{head}, the PR's head now, is {st['pushedBy'][1]}'s push, not its own"


def unposted(st, kind, at):
    """Why the last job of the kind on head `at` left answers that didn't go
    up, or None: for the next job of the kind's brief."""
    got = st.get(kind + "Unposted")
    return got["why"] if got and got["at"] == at else None
