"""One PR's next step, for autoland-steward's pass (steward.py): where it
stands (its reviewers' verdicts, CI, its threads, its labels, what GitHub
says of its merge), and the thing to do now: queue a review, post what came
back, hand what's open (findings, red CI, a conflict with its base) to a
Claude job, have a job run its machine checks or judge it, post its
`review-settled` status and merge it (or, in a repo with its merge train
on, put it in the train, which lands it: train.py), or list it for the
owner or the stuck issue. Each step it takes is in the PR's status, and in
what the watchdog watches (waits.py).

A step that fails (GitHub refuses it, or no job can start) is tried again,
later each time, and never quietly for ever: the third failure in a row on a
head makes the PR's wait overdue, so the watchdog takes it (strike). A Claude
job that stops before it finishes (it times out, goes quiet or crashes) is
the machine failing, not the job's answer: it's started again the same way
(retry_at)."""

import os
import re
import time

import common as C
import proofs as P
import reviewers as V
import train as T
import waits
from common import CFG, DRY, MIN, STATE, load, log, repo_cfg, save, ts
from seal import MAJOR as MAJOR_VERDICT  # renovate-sweep's, on a Renovate major
from touches import bot_edited, dep_kind, gate_edit, moved_from, one_way
from tasks import DEP_SEEN, JUDGMENT, PAST_LIMIT, REVIEWED, fix_task, judge_task, proof_task
from prs import MERGE_STEPS, ci_state, eyes, land, our_status, post_status

# The link Claude Code puts in a PR its session opens.
SESSION = re.compile(r"https://claude\.ai/code/session_\w+")
# What a fix job of each kind is on, for a PR's status while another waits.
ON = {"handed": "its findings", "redFixed": "its red CI", "failFixed": "what its judge found failing",
      "conflictFixed": "its conflicts with its base"}
# The steps that fail and are tried again (strike), and what a run of
# failures says, on the PR's wait and the stuck issue.
STRIKES = {
    "status": "GitHub keeps refusing its status",
    "update": "GitHub keeps refusing to bring it up to date",
    "merge": "GitHub keeps refusing the merge",
    "mergeable": "GitHub won't say it can be merged",
    "job": "no Claude job can start for it",
    "post": "GitHub keeps refusing a review it's to post",
    "lost": "its Claude jobs keep stopping before they finish (timed out, quiet or crashed)",
}
STRIKE_LIMIT = 3  # failures in a row of one step, on one head, that make its wait overdue


def clock(t):
    """A time in a PR's status, in the machine's own time zone: 6:47pm PDT."""
    return time.strftime("%-I:%M%p %Z", time.localtime(t)).replace("AM", "am").replace("PM", "pm")


def handle(repo, pr, repo_info, digest, obsolete=False):
    rc = repo_cfg(repo)
    n, head = pr["number"], pr["headRefOid"]
    by_train = T.rides(repo, pr, repo_info)
    # The judge reads the diff against the base, so its verdict (and its
    # job) is for this head on this base: one retargeted is judged again.
    judged_at = f"{head} {pr['baseRefName']}"
    key = f"pr-{repo.replace('/', '_')}-{n}.json"
    st = load(key, {})
    # Findings a reviewer couldn't post: by reviewer, then base (on another
    # base the PR is another change), the head they were on, until it posts
    # a look at the PR's head on that base. Kept across heads, since past
    # the round limit no new head's look would cover them. State from
    # before kept the loss in its head's rv: moved here before a new head
    # clears that.
    lost_on = st.setdefault("lostOn", {})
    for r, rs in st.get("rv", {}).items():
        if rs.get("lost"):
            was, base = rs.pop("lost")
            lost_on.setdefault(r, {})[base] = was
    if st.get("head") != head:
        st.update(head=head, seen=C.NOW, handed=None, strikes={}, rv={})
    st.setdefault("rv", {})
    labels = {x["name"] for x in pr["labels"]["nodes"]}
    merging = MERGE_STEPS.get(pr.get("mergeStateStatus"), "refused")
    age = C.NOW - st["seen"]
    tag = f"{repo}#{n}"
    line = f"[{repo}#{n}]({pr['url']}) {pr['title']}"

    struck = {}  # the steps that failed this pass, and when each is tried again
    ends = {}  # how this head's job of each kind ended, as proj says (running)
    looked = {}  # what running said of each job this pass, by its id

    def after(t, minutes):
        """A wait that ends `minutes` after t: this repo gets a pass then."""
        C.wake(repo, t + minutes * MIN + 1)  # ends: as each call says

    def strike(kind, said=None):
        """The step (STRIKES) failed, GitHub saying why when it said: tried
        again on a pass booked later each time, a minute on, then two,
        four... up to an hour, so a lasting failure costs little. Its
        STRIKE_LIMIT-th failure in a row on this head makes the PR's wait
        overdue whatever its deadline, so the watchdog takes it (a kick, a
        Claude job, the stuck issue); without the watchdog, it's listed on
        the stuck issue. Either says what GitHub said. Once a pass: two
        reviews it couldn't post are one pass's failure. When it's tried
        again."""
        if kind in struck:
            return struck[kind]
        strikes = st.setdefault("strikes", {})
        strikes[kind] = times = strikes.get(kind, 0) + 1
        log(f"{tag}: {STRIKES[kind]} ({times} in a row){f': {said}' if said else ''}")
        minutes = min(2 ** (times - 1), 60)
        struck[kind] = C.NOW + minutes * MIN
        after(C.NOW, minutes)  # ends: STRIKE_LIMIT in a row make the wait overdue
        if times >= STRIKE_LIMIT and not waits.overdue(repo, pr, STRIKES[kind], said):
            digest.append(("Failing again and again", line, f"{STRIKES[kind]}, {times} times in a row"
                           + (f": {said}" if said else " (journalctl -u autoland)")))
        return struck[kind]

    def cleared(kind):
        """The step went through: its run of failures is over, unless it
        failed this pass too (one review posted, another not)."""
        if kind not in struck:
            st.setdefault("strikes", {}).pop(kind, None)

    # Open a day, it's listed for the owner (below): a pass then says so.
    after(ts(pr["createdAt"]), 24 * 60)  # ends: listed for the owner

    session = bool(SESSION.search(pr.get("body") or ""))

    def handoff_at():
        """When findings or red CI will have sat long enough for a Claude job
        to take them (a pass is booked for then). A PR that links the Claude
        session that opened it is that session's to settle: it hears of
        every finding and failure and is likely at work on them, so a job
        takes over only after a longer quiet, in case the session is gone.
        Every push or reply starts either wait over."""
        wait = rc["sessionHandoffAfter"] if session else rc["handoffAfter"]
        since = max(ts(pr["updatedAt"]), st["seen"])
        after(since, wait)  # ends: a Claude job takes it then
        return since + wait * MIN

    def settle(state, desc, why=None, wait=waits.STEWARD):
        """The PR's status, and what it waits on for the watchdog (waits.py).
        False if GitHub didn't take it."""
        ok = post_status(repo, pr, state, desc)
        if why:
            digest.append((why, line, desc))
        waits.note(repo, pr, desc, wait)
        # Not settled (any more): nothing may merge it.
        if state != "success" and (C.queued(pr) or st.get("landed")):
            if C.unland(repo, pr):
                st["landed"] = None
            else:
                ok = False
        if ok or DRY:
            cleared("status")
        else:
            # GitHub didn't take the status or the stop: until it does, the
            # old ones stand and a merge queued on it can still go.
            strike("status")
        # Nor may its train land it. Out after the status: cn reads it right
        # before each landing and takes out a head the steward no longer
        # says is settled (Conary#55). Still in, with the old status
        # standing, its train isn't stepped.
        if state != "success" and T.seat(repo, n) is not None and not T.leave(repo, pr) and not ok:
            T.stop(repo, f"#{n} isn't settled, and GitHub hasn't taken its status")
        return ok

    def update_branch():
        """Bring a settled head up to date with a base whose rules want it
        (the merge won't): the merged-in head is reviewed like any other.
        GitHub makes it a moment later, and its push kicks a pass; one that
        hasn't come ten minutes on is asked for again. True while it's being
        brought up to date, False if it isn't behind, None if GitHub didn't
        take the ask (strike)."""
        if merging != "update":
            return False
        if st.get("updated") == head and C.NOW - st.get("updatedAt", 0) < 10 * MIN:
            after(st["updatedAt"], 10)  # ends: a wait that stays the same is overdue at its deadline
            return True
        try:
            C.write("api", "-X", "PUT", f"repos/{repo}/pulls/{n}/update-branch", "-f", f"expected_head_sha={head}")
        except RuntimeError as e:
            strike("update", C.said(e))
            return None
        cleared("update")
        log(f"{tag}: behind its base, brought up to date")
        st["updated"], st["updatedAt"] = head, C.NOW
        after(C.NOW, 10)  # ends: a wait that stays the same is overdue at its deadline (waits.py)
        return True

    def deadline(kind):
        """A pass when this kind's job is due to have ended (proj stops a turn
        at jobTimeout), and every quarter hour past that while it still
        hasn't (it waited for a slot first): its end kicks one, and this is
        the one that sees it if that kick never got through."""
        due = st.get(kind + "At", C.NOW) + (CFG["jobTimeout"] + 5) * MIN
        C.wake(repo, due if due > C.NOW else C.NOW + 15 * MIN)  # ends: its PR's wait, unchanged, is overdue at its deadline

    def running(kind, at=head):
        """This head's job of the kind (at: on its base, for the judge's and
        the fix it asks for) while it's still queued or running, or proj
        can't say: "queued", "running" or "unknown"; else "", and how it
        ended goes in ends. One that is keeps its deadline, since each pass
        replaces the repo's wake-ups. One look a job a pass: asked again,
        it says the same, and a run of broken jobs is counted once
        (breaking)."""
        jid = st.get(kind + "Job")
        if st.get(kind) != at or not jid:
            return ""
        if jid not in looked:
            looked[jid] = look(kind, jid)
        return looked[jid]

    def look(kind, jid):
        """running's look at the job: what proj says of it, once."""
        state = C.job_state(jid)
        live = state in (None, "queued", "running")
        if not live:
            ends[kind] = state
        if live or broke(kind):
            breaking(kind)
        if not live:
            return ""
        deadline(kind)
        return state or "unknown"

    def at_work(kind, doing, at=head):
        """What the status says of this head's job of the kind: queued for
        its work until proj starts it (a job can wait hours for a slot, and
        proj kicks a pass when it starts), at it from then on."""
        if running(kind, at) == "queued":
            return f"a Claude job is queued for {doing}"
        return f"a Claude job is {doing}"

    def started(kind, task, at=head, broken=False):
        """A Claude job, once per head (at: once per head on its base, for
        the judge's; recorded once it has started; until then it's tried
        again (strike), and its caller lists it on the stuck issue). One
        that isn't in place of one that broke (retry_at) ends a run of
        those."""
        jid = C.job(repo, task)
        if jid:
            st[kind] = at
            st[kind + "Job"] = jid
            st[kind + "At"] = C.NOW
            deadline(kind)
            cleared("job")
            if not broken:
                cleared("lost")
            log(f"{tag}: a Claude job {jid} ({kind})")
        else:
            log(f"{tag}: couldn't start a Claude job (no checkout, or proj refused)")
            strike("job")
        return bool(jid)

    def lost(kind, at=head):
        """This head's job of the kind ended without the answer it was for
        (it crashed, timed out, or never posted it). Seen on two passes: the
        first may have read the PR just before its answer went up."""
        if st.get(kind) != at or not st.get(kind + "Job") or running(kind, at):
            return False
        seen = st.get(kind + "Lost") == at
        st[kind + "Lost"] = at
        if not seen:
            after(C.NOW, 1)  # ends: the second look, once
        return seen

    def broke(kind):
        """How this head's job of the kind ended, if it stopped before it
        finished its turn (it timed out, went quiet, crashed, or Claude
        errored), else None: that's the machine failing, not an answer."""
        how = ends.get(kind)
        if how in (None, "done"):
            return None
        return {"error": "ended in an error", "failed": "crashed"}.get(how, how)

    def retry_at(kind):
        """When this head's job of the kind that broke may start again: each
        such job is a strike, never counted as an answer, so the third on a
        head makes its wait overdue (the watchdog takes it), and the next
        starts on the pass the strike booked, a minute on, then two, four...
        up to an hour. Each pass replaces the repo's wake-ups, so every one
        that waits for it books that pass again."""
        if st.get(kind + "Broke", [None])[0] != st.get(kind + "At"):
            st[kind + "Broke"] = [st.get(kind + "At"), strike("lost")]
        breaking(kind)
        due = st[kind + "Broke"][1]
        if due > C.NOW:
            after(due, 0)  # ends: the retry, on its pass
        return due

    def breaking(kind):
        """Jobs on this head keep breaking: STRIKE_LIMIT or more in a row,
        this kind's among them. While its step is still this kind's job (one
        in place of one that broke, waiting to start or running, or another
        that broke), its wait stays overdue, as a step failing on every pass
        does (strike): the watchdog takes it, however long the retries go
        on. One that finishes its turn ends the run (started)."""
        times = st.get("strikes", {}).get("lost", 0)
        if times < STRIKE_LIMIT or not st.get(kind + "Broke") or "lost" in struck:
            return
        if not waits.overdue(repo, pr, STRIKES["lost"]):
            digest.append(("Failing again and again", line, f"{STRIKES['lost']}, {times} times in a row (journalctl -u autoland)"))

    def again(kind, task, doing, answer, at=head):
        """A job that ended without its answer is started once more; one that
        does that twice fails the PR's status and is listed for the owner.
        One that broke is started again once its retry is due, however many
        times, and doesn't count toward the two (retry_at)."""
        how = broke(kind)
        if how and (due := retry_at(kind)) > C.NOW:
            settle("pending", f"Settled; the last Claude job {doing} {how}, so another starts at {clock(due)}"[:140])
        elif not how and st.get(kind + "Again") == at:
            settle("failure", f"Settled, but two Claude jobs ended without {answer}"[:140],
                   f"Two Claude jobs ended without {answer}")
        elif started(kind, task, at, bool(how)):
            if not how:
                st[kind + "Again"] = at
            st.pop(kind + "Lost", None)
            settle("pending", f"Settled; {at_work(kind, doing, at)} again (the last one {how or f'ended without {answer}'})"[:140])
        else:
            settle("pending", f"Settled, but no Claude job could start {doing} again"[:140],
                   "Couldn't start a Claude job")

    def fixing():
        """The kind of fix job still queued or running (or that proj can't
        say about), else None: whatever head or base it was handed on, since
        each pushes to the branch, and a new head or base doesn't stop it."""
        for k in ON:
            if st.get(k + "Job") and running(k, st.get(k)):
                return k
        return None

    def hand_off(why, desc, kind="handed", at=head, wait=True, ours=True):
        """Pending on desc while a Claude job fixes why, one of each kind a
        head (at), and the status says who has it: that job and since when,
        the Claude session that opened the PR and until when, or when a job
        takes it. A job does once it has sat long enough (handoff_at), or at
        once if it isn't to wait; never if it isn't ours to hand (a Renovate
        update renovate-sweep has). One that finished its turn with this
        still to fix (it pushed nothing) is handed once more; after a second
        the status fails and the owner hears, until a new head. One that
        broke (it crashed, timed out or went quiet) is handed again once its
        retry is due, and doesn't count toward the two (retry_at). Never
        while another fix job is still working on this head: two on one
        branch push over each other."""
        retry = st.get(kind) == at
        state = retry and running(kind, at)
        gone = retry and not state and lost(kind, at)
        how = gone and broke(kind)
        if gone and not how and st.get(kind + "Again") == at:
            settle("failure", f"Two Claude jobs ended without fixing it: {desc}"[:140],
                   "Two Claude jobs ended without fixing it")
            return
        if state:
            who = f"a Claude job {'is queued for it' if state == 'queued' else 'has it'} since {clock(st[kind + 'At'])}"
        elif retry and not gone:
            who = "its Claude job just ended"  # the second look is booked
        elif how and (due := retry_at(kind)) > C.NOW:
            who = f"the last Claude job on it {how}, so another takes it at {clock(due)}"
        elif not ours:
            who = ""
        elif wait and (due := handoff_at()) >= C.NOW:
            who = (f"the Claude session that opened it has it until {clock(due)}, then a Claude job" if session
                   else f"a Claude job takes it at {clock(due)}")
        elif busy := fixing():
            log(f"{tag}: a Claude job ({busy}) is still working on this head; {kind} waits for it")
            who = f"a Claude job takes it once the one on {ON[busy]} ends"
        else:
            proofs = {c: rc["proofJobs"][c] for c in ci_names if c in rc["proofJobs"]} if ci == "red" else {}
            train = ci == "red" and any(c.startswith(f"{T.CONTEXT} (") for c in ci_names)
            if started(kind, fix_task(repo, pr, why, proofs, train), at, bool(how)):
                if retry:
                    if not how:
                        st[kind + "Again"] = at
                    st.pop(kind + "Lost", None)
                if kind == "handed":
                    eyes(repo, pr)
                who = f"a Claude job has it since {clock(C.NOW)}"
            else:
                digest.append(("Couldn't start a Claude job to fix it", line, why))
                who = "no Claude job could start"
        settle("pending", f"{desc}; {who}" if who else desc)

    def merge():
        # Marked landed only once GitHub has done the merge, so a failed one
        # is tried again (strike), saying why.
        refused = land(repo, pr, repo_info)
        if refused is None:
            st["landed"] = head
            cleared("merge")
            return
        strike("merge", refused)

    def land_now(desc):
        """Settled: merged, unless what GitHub says of its merge asks for
        something first (MERGE_STEPS). Behind a base whose rules want it up
        to date, it's brought up to date instead, and the merged-in head is
        reviewed and its CI run like any other before it lands. (One that
        conflicts with its base never gets here: it's handed on first.) In a
        repo with its merge train on, one against the default branch goes in
        the train instead (train.py's ride)."""
        if merging == "update":
            why = "brought up to date" if update_branch() else "GitHub didn't take the update; trying again"
            settle("pending", f"Settled, but behind its base: {why}")
            return
        if by_train:
            # The train merges it onto the default branch itself, so what
            # GitHub says of merging it alone is no wait of its own: GitHub
            # works that out afresh each time the branch moves, which the
            # train's every landing does.
            T.ride(repo, pr, st, bool(gate), desc, settle, strike, our_status(pr, T.CONTEXT)[1])
            return
        if merging in ("wait", "refused"):
            settle("pending", "Settled; GitHub is still working out whether it can be merged" if merging == "wait"
                   else f"Settled, but GitHub won't say it can be merged ({pr.get('mergeStateStatus')})")
            strike("mergeable")
            return
        cleared("mergeable")
        if T.seat(repo, n) is not None:
            # Retargeted since it joined: the train lands only onto the
            # default branch, so it comes out and is merged here.
            T.leave(repo, pr)
        settle("success", desc)
        # Still open with a merge queued on it: GitHub's to merge later, which
        # land never leaves it to (a steward before it did). Merged now instead.
        if st.get("landed") == head and C.queued(pr):
            st["landed"] = None
        if st.get("landed") != head:
            log(f"{tag}: settled, landing")
            merge()

    def conflicted():
        """It conflicts with its base, so GitHub can't merge it and its CI
        doesn't run: a Claude job merges the base in, at once (one a head on
        its base, so one retargeted gets its own), and its push is reviewed
        like any other head. A dependency bot's own branch is the
        bot's to rebase, and only its own: one with anyone else's commit on
        it the bot leaves alone, major or minor. One renovate-sweep has (CI
        red, or a major with no verdict on this head) is its job's to bring
        up to date, and one it held is the owner's: no second job pushes to
        either branch."""
        base = pr["baseRefName"]
        verdict = C.marker(pr, MAJOR_VERDICT, head, repo, base) if dep == "major" and renovate else None
        if dep and not bot_edited(repo, pr, st):
            settle("pending", f"Conflicts with {base}; its bot rebases it")
            open_a_day(f"Dependency update; conflicts with {base}", swept=renovate)
        elif verdict == "held":
            settle("pending", "Major update; renovate-sweep held it: see its comment", wait=None)
        elif renovate and dep and (ci == "red" or dep == "major" and verdict != "ok"):
            settle("pending", f"Conflicts with {base}; renovate-sweep has it", wait=waits.SWEEP)
        else:
            hand_off(f"it conflicts with {base}, so GitHub can't merge it and its CI doesn't run: merge {base} into the "
                     "branch (a merge commit: never rebase or force-push), resolve every conflict keeping what both sides "
                     "meant, and run the repo's tests before you push. If the two sides can't both be kept, which one stays "
                     f"is the owner's call. Just before you merge, and again before you push, check that the PR is still open "
                     f"on {base} at head {head}: if its base or head moved on, stop without merging or pushing, since the "
                     "steward hands what it is now to a new job", f"Conflicts with {base}", "conflictFixed", judged_at,
                     wait=False)
            # A job stuck in line, or one whose state can't be read, doesn't
            # keep it off the list.
            open_a_day(f"Conflicts with {base}")

    def open_a_day(what, swept=False):
        """Anything not merged a day after it was opened, reviewed clean or
        not (a check that never reports keeps it pending), is listed, unless
        it's listed already, watchdog or not. A Renovate update's own stuck
        ones are renovate-sweep's (swept)."""
        if not swept and C.NOW - ts(pr["createdAt"]) > 86400 and not any(x[1] == line for x in digest):
            digest.append(("Open over a day", line, what))

    def owner_decides():
        """Sent to the owner by hand, by a fix job or by a judge (the label):
        listed for them, and no judge job."""
        judged_owner = C.marker(pr, JUDGMENT, head, repo) == "owner"
        if judged_owner:
            # The judge's word, seen labelled on this head: once the label
            # is off, that's the owner's go-ahead. A label taken off before
            # the judge said anything isn't an answer to it.
            st["ownerSaw"] = judged_at
        settle("pending", f"Settled; {CFG['owner']} decides (the {CFG['labels']['owner']} label)",
               "Judged yours to decide" if judged_owner else f"Labelled {CFG['labels']['owner']}")

    def judged(why, seen=REVIEWED):
        """Settled, but not something the steward lands on its own word: one
        Claude job a head judges it, and its verdict on this head decides."""
        verdict = C.marker(pr, JUDGMENT, head, repo)
        if verdict == "land":
            land_now(f"Settled; a Claude job judged it fine to land ({why})")
        elif verdict == "owner" and st.get("ownerSaw") == judged_at:
            land_now(f"Settled; a Claude job sent it to {CFG['owner']}, who took the {CFG['labels']['owner']} label off ({why})")
        elif verdict == "owner":
            # Labelled here, never by the judge, so the steward knows it went
            # on: the owner has a label to take off, and taking it off is
            # their answer even before a pass has seen it on.
            settle("pending", f"Settled; a Claude job judged it {CFG['owner']}'s to decide ({why})", "Judged yours to decide")
            if C.write("pr", "edit", str(n), "-R", repo, "--add-label", CFG["labels"]["owner"], check=False) is not None:
                st["ownerSaw"] = judged_at
        elif verdict == "failed":
            # The judge's own build or tests failed on this head: a fix job
            # has it, and its push is judged again. Its own once a head: a
            # job that answered a finding here without pushing didn't fix this.
            hand_off("the Claude job that judged it found something failing on this head, its build or tests or a finding "
                     "left unfixed (its comment on the PR says what)",
                     "Settled, but its build or tests fail", "failFixed", judged_at, wait=False)
        elif update_branch():
            settle("pending", "Settled, but behind its base: brought up to date, reviewed again")
        elif lost("judged", judged_at):
            again("judged", judge_task(repo, pr, why, ci == "none", seen), "judging it", "a verdict", judged_at)
        elif st.get("judged") == judged_at or started("judged", judge_task(repo, pr, why, ci == "none", seen), judged_at):
            settle("pending", f"Settled; {at_work('judged', 'judging it', judged_at)} ({why})")
        else:
            settle("pending", f"Settled, but no Claude job could judge it ({why})", "Couldn't start a Claude job to judge it")

    def prove():
        """Only checks that run on a machine, not in CI, are left: a Claude
        job runs them on this head and records their verdicts, once its
        repo's proofSlots have room for it (a job started again too), or it
        has nothing new to build (proofs.py)."""
        doing = "running " + ", ".join(ci_names)
        task = proof_task(repo, pr, ci_names, rc["proofJobs"])
        retry = lost("proved")

        def running_now():
            skipped = "; nothing new to build, so it skipped the line" if st.get("provedFree") == head else ""
            settle("pending", f"Settled; {at_work('proved', doing)}{skipped}"[:140])

        if not retry and st.get("proved") == head:
            running_now()
            return
        # One that broke is counted, and its retry booked, whether or not
        # there's room: only the job started in its place waits for a slot.
        if retry and broke("proved"):
            retry_at("proved")
        slots = rc.get("proofSlots")
        outs = P.outputs(repo, pr, ci_names, st) if slots else None
        free = False
        # One whose next step is failing the PR (again) needs no slot.
        last = retry and st.get("provedAgain") == head and not broke("proved")
        if slots and not last and (ahead := P.ahead(repo, n, st, slots, outs)) is not None:
            if ahead != P.FREE:
                P.wait(repo, n)
                settle("pending", f"Settled; waiting its turn to run {', '.join(ci_names)} ({ahead} ahead)"[:140])
                return
            free = True
        before = st.get("provedJob")
        st["provedFree"] = head if free else None
        if retry:
            again("proved", task, doing, "its verdict")
        elif started("proved", task):
            running_now()
        else:
            settle("pending", f"Settled, but no Claude job could run {', '.join(ci_names)}"[:140],
                   "Couldn't start a Claude job to run its checks")
        P.started(st, outs if st.get("provedJob") != before else None)

    if pr["isDraft"]:
        # Work in progress, whoever's it is: nobody reviews it yet, and nobody
        # is told.
        settle("pending", "Draft: work in progress, reviewed once it's marked ready", wait=None)
        save(key, st)
        return
    if not C.acted_on(pr):
        settle("pending", f"Outside contribution: {CFG['owner']} reviews and merges it", "Outside contributions")
        save(key, st)
        return
    # A list longer than one page could hide a hold, an open thread, a
    # changes-requested review or a verdict: fail closed. (open_prs reads
    # older reviews and comments, so those two are cut only past its cap.)
    cut = [k for k, more in (("labels", pr["labels"]["pageInfo"]["hasNextPage"]),
                             ("review threads", pr["reviewThreads"]["pageInfo"]["hasNextPage"]),
                             ("reviews", pr["reviews"]["pageInfo"]["hasPreviousPage"]),
                             ("comments", pr["comments"]["pageInfo"]["hasPreviousPage"])) if more]
    if cut:
        settle("pending", f"Too many {' and '.join(cut)} to read: {CFG['owner']} merges it", "Too much to read")
        save(key, st)
        return
    if obsolete:
        # A red-branch fix its branch went green without (R.watch_default),
        # waiting to be closed: like a hold, nothing may merge it.
        settle("pending", f"Not needed: {pr['baseRefName']} is green again without it")
        save(key, st)
        return
    if CFG["labels"]["hold"] in labels:
        # Held after it was settled: take back the status and the queued merge.
        log(f"{tag}: on hold")
        settle("pending", f"On hold (the {CFG['labels']['hold']} label)", wait=waits.HOLD)
        save(key, st)
        return

    ci, ci_names = ci_state(pr, rc["proofJobs"])
    # No checks ran at all ("none"). Unless the repo is set not to need any
    # (docs only), that's not green: nothing has tested it, so a judge job
    # runs the build and tests itself. One the train lands, the train's CI
    # is what tests it, merged onto the default branch.
    if ci == "none" and (not rc["requireCi"] or by_train):
        ci = "green"
    open_threads = sum(1 for t in pr["reviewThreads"]["nodes"] if not t["isResolved"])
    latest = {}
    for r in pr["reviews"]["nodes"]:
        who = (r["author"] or {}).get("login")
        if who and r["state"] in ("APPROVED", "CHANGES_REQUESTED", "DISMISSED"):
            latest[who] = r["state"]
    changes = [w for w, s in latest.items() if s == "CHANGES_REQUESTED"]
    dep = dep_kind(pr)
    gate = one_way(pr, rc, moved_from(repo, pr, st))
    renovate = pr["headRefName"].startswith("renovate/")
    proof_only = ci == "pending" and bool(ci_names) and all(c in rc["proofJobs"] for c in ci_names)
    # A minor dependency PR a Claude job fixed carries Claude's code, which no
    # reviewer has read (and Renovate won't merge); a bot's PR that changes a
    # workflow beyond repinning its actions changes the gate. Either goes
    # through the review loop below like a Claude PR. While a Renovate one's CI
    # is red it's renovate-sweep's to hand over, findings and all: a job from
    # each on one branch would push over each other. The sweep's push makes a
    # new head, and the reviewers read it again.
    # A judge asked for on this head and base (the repo had no CI then, say)
    # still decides once CI shows up: its owner or failed verdict isn't
    # dropped for a green check.
    judge_said = C.marker(pr, JUDGMENT, head, repo)
    asked = st.get("judged") == judged_at or judge_said is not None
    # Its owner or failed verdict holds it whatever CI is still doing; only
    # a land waits for CI.
    held = asked and judge_said in ("owner", "failed")
    fixed = dep == "minor" and not gate and (bot_edited(repo, pr, st) or gate_edit(repo, pr, st))
    swept = fixed and ci == "red" and renovate

    # Dependency bumps: CI is the review. Renovate merges its own minor
    # ones; renovate-sweep fixes the red ones (and settles their threads) and
    # gives each major a verdict. Anything else that's open on one, a review
    # finding or a red Dependabot update, goes to a Claude job like any PR's.
    # Sent to the owner (by hand, a fix job or a judge): theirs, whatever else
    # is open on it, and nothing more is asked or handed off until the label
    # comes off.
    if CFG["labels"]["owner"] in labels:
        owner_decides()
        save(key, st)
        return
    # Conflicting with its base, nothing else on it can settle: GitHub won't
    # merge it and its CI doesn't run.
    if merging == "conflict":
        conflicted()
        save(key, st)
        return

    if dep and not fixed:
        major_verdict = C.marker(pr, MAJOR_VERDICT, head, repo, pr["baseRefName"]) if dep == "major" and renovate else None
        threads = f"{open_threads} open review thread{'s' * (open_threads > 1)}" if open_threads else ""
        if changes:
            threads = ", ".join([threads] * bool(threads) + ["changes requested by " + ", ".join(changes)])
        if renovate and ci == "red":
            settle("pending", "Dependency update, CI red; renovate-sweep has it" + (f" ({threads})" if threads else ""),
                   wait=waits.SWEEP)
        elif renovate and dep == "major" and threads and not major_verdict:
            # Until it gives this head a verdict, renovate-sweep's job has it,
            # threads, reviews and all: one job on its branch.
            settle("pending", f"Major update, {threads}; renovate-sweep has it"[:140], wait=waits.SWEEP)
        elif renovate and dep == "major" and major_verdict == "held":
            # Held for the owner (renovate-sweep tells them): no job pushes
            # to it, threads or not, and changes what they're deciding on.
            settle("pending", "Major update; renovate-sweep held it: see its comment", wait=None)
        elif threads or ci == "red":
            what = threads or "CI is red (" + ", ".join(ci_names) + ")"
            kind = "handed" if threads else "redFixed"
            hand_off(what, f"Dependency update; {what}", kind)
        elif dep == "major" and renovate and major_verdict != "ok":
            # renovate-sweep's verdict comes first, and its job runs the proofs
            # until then: neither a judge nor a proof job of the steward's
            # works the branch beside it.
            settle("pending", "Major update; waiting for renovate-sweep's verdict on this head", wait=waits.SWEEP)
        elif held:
            judged("a Claude job was asked to judge this head", DEP_SEEN)
        elif proof_only:
            prove()
        elif ci == "pending":
            settle("pending", "Dependency update, CI pending")
        elif gate or ci == "none" or asked:
            judged(f"a dependency update touching {gate}" if gate else "the repo has no CI" if ci == "none"
                   else "a Claude job was asked to judge this head",
                   DEP_SEEN + ("; renovate-sweep cleared the major version for this head" if major_verdict == "ok" else ""))
        elif dep == "major" and not renovate:
            judged("a major dependency update", DEP_SEEN)
        elif dep == "major":
            land_now("Major update renovate-sweep cleared, CI green")
        elif renovate and not by_train:
            # Renovate merges its own, except the ones the train lands.
            settle("success", "Dependency update, CI green")
        else:
            land_now("Dependency update, CI green")
        open_a_day(f"Dependency update, CI {ci}", swept=renovate)
        save(key, st)
        return

    # The reviewers (reviewers.py): every one this repo has, but one out of
    # service (its usage spent, say) isn't waited for meanwhile. Each review
    # is queued for the reviewers' own service; a pass posts what comes back.
    names = V.on(repo)
    # What the reviewers weren't shown: their clean reviews don't cover it,
    # so a judge, who sees it all, looks before it lands.
    unseen = V.unseen(key, head, pr["baseRefName"])
    away = {r: V.out(r) for r in names}
    live = [r for r in names if not away[r]]
    for r in names:
        # Once a pass, whichever PR finds it out, while the others carry on
        # without it (with none left, the PR says so below): logging it back
        # in, say, is the owner's to do.
        if live and away[r] and not any(x[:2] == ("A reviewer is out of service", V.NAMES[r]) for x in digest):
            digest.append(("A reviewer is out of service", V.NAMES[r], away[r]))
    said = {r: V.verdict(pr, head, r) for r in names}  # verdict, when, rounds with findings
    found = []  # the reviewers whose findings on this head this pass posted
    late, unposted = [], []  # earlier heads whose findings this pass posted; reviews it couldn't post yet

    def went_out(r, back):
        o = load(V.OUT, {})
        o[r] = {"why": back["why"], "until": back["until"]}
        save(V.OUT, o)
        log(f"{tag}: {V.NAMES[r]} is out of service until {back['until']:.0f}: {back['why']}")

    def outs(r):
        """An earlier head's look that found the reviewer out of service:
        recorded before anything this pass decides on whether it's out."""
        for name in V.answered(key, r):
            back = load(f"{V.DONE}/{name}.json", None) or {}
            if name != V.item_name(key, r, head, pr["baseRefName"]) and back.get("result") == "out" and not DRY:
                went_out(r, back)
                V.drop(name)

    def consume(r):
        """What came back from the reviewer's service for this head, if
        anything: posted as its review, or counted as a failure."""
        rs = st["rv"].setdefault(r, {})
        name = V.item_name(key, r, head, pr["baseRefName"])
        back = load(f"{V.DONE}/{name}.json", None)
        if back and not DRY:
            if back["result"] == "review":
                try:
                    v = V.post(repo, pr, r, back["answer"])
                    _, _, rr = said[r]
                    said[r] = (v, C.NOW, rr + (v == "findings"))
                    if v == "findings":
                        found.append(r)
                    rs.pop("fails", None)
                    rs.pop("failedAt", None)
                    cleared("post")
                    # A look at the PR's head covers what one at it or an
                    # earlier head on its base found.
                    lost_on.get(r, {}).pop(pr["baseRefName"], None)
                    if not lost_on.get(r, True):
                        lost_on.pop(r)
                except ValueError as e:  # an answer that's no review
                    rs["fails"], rs["failedAt"] = rs.get("fails", 0) + 1, C.NOW
                    log(f"{tag}: {V.NAMES[r]}'s answer wasn't a review ({rs['fails']}): {e}")
                except C.Unavailable as e:  # GitHub didn't answer: no try spent, the answer's kept
                    log(f"{tag}: couldn't post {V.NAMES[r]}'s review, GitHub's unavailable: {e}")
                    unposted.append(f"{V.NAMES[r]}'s review of {head[:7]}")
                    strike("post", C.said(e))
                    return
                except RuntimeError as e:  # GitHub said no: the same answer goes again next pass
                    rs["fails"], rs["failedAt"] = rs.get("fails", 0) + 1, C.NOW
                    log(f"{tag}: couldn't post {V.NAMES[r]}'s review ({rs['fails']}): {e}")
                    # Three tries; one out of service then isn't asked again,
                    # so its answer stays, the pass waits and the owner hears.
                    if rs["fails"] < 3 or V.out(r):
                        x = f"{V.NAMES[r]}'s review of {head[:7]}"
                        unposted.append(x)
                        if rs["fails"] >= 3:
                            digest.append(("A review couldn't be posted", line, f"{x}, {rs['fails']} tries"))
                        strike("post", C.said(e))
                        return
                    if back["answer"].get("findings"):
                        # Dropped, it's asked again; one out of service by
                        # then isn't, so the head's held for it meanwhile.
                        lost_on.setdefault(r, {})[pr["baseRefName"]] = head
            elif back["result"] == "out":
                went_out(r, back)
            else:  # the agent, the network, its JSON: tried again next pass
                rs["fails"], rs["failedAt"] = rs.get("fails", 0) + 1, C.NOW
                log(f"{tag}: {V.NAMES[r]}'s review failed ({rs['fails']}): {back.get('error', '')[:200]}")
            V.drop(name)
            rs.pop("queued", None)
            rs.pop("gone", None)
        elif rs.get("queued") and not DRY and not os.path.exists(os.path.join(STATE, V.QUEUE, name + ".json")):
            # Out of the queue with nothing back. However long an agent
            # thinks, its review stays queued until it's answered, so this
            # is a lost one; seen on two passes, since the service may have
            # answered between this pass's two looks.
            if rs.get("gone"):
                rs.pop("queued")
                rs.pop("gone")
                rs["fails"], rs["failedAt"] = rs.get("fails", 0) + 1, C.NOW
                log(f"{tag}: the queued {V.NAMES[r]} review left the queue without an answer")
            else:
                rs["gone"] = True
                after(C.NOW, 1)  # ends: the second look, once

    def overtaken(r):
        """What came back from the reviewer's looks at the PR's earlier heads
        after it moved on (V.sweep keeps them): posted on the commit each
        one read, so what it found goes into the next push instead of
        waiting for its look at this head. Findings there count as a round,
        and hold this pass back from landing: the threads it read are from
        before them. "Nothing found" there says nothing about this head."""
        rs = st["rv"].setdefault(r, {})
        mine = V.answered(key, r)
        tries = {n: k for n, k in rs.get("late", {}).items() if n in mine}
        for name in mine:
            if name == V.item_name(key, r, head, pr["baseRefName"]):
                continue
            back = load(f"{V.DONE}/{name}.json", None) or {}
            if DRY:
                continue
            if back.get("result") == "out":  # in since outs() looked
                went_out(r, back)
            elif back.get("result") == "review" and back.get("base") == pr["baseRefName"] and isinstance(back.get("head"), str):
                try:
                    if V.post(repo, {**pr, "headRefOid": back["head"]}, r, back["answer"]) == "findings":
                        v, at, rr = said[r]
                        said[r] = (v, at, rr + 1)
                        late.append(back["head"][:7])
                    cleared("post")
                except ValueError as e:  # an answer that's no review
                    log(f"{tag}: {V.NAMES[r]}'s review of {back['head'][:7]} wasn't a review: {e}")
                except C.Unavailable as e:  # GitHub didn't answer: no try spent, the answer's kept
                    log(f"{tag}: couldn't post {V.NAMES[r]}'s review of {back['head'][:7]}, GitHub's unavailable: {e}")
                    unposted.append(f"{V.NAMES[r]}'s review of {back['head'][:7]}")
                    strike("post", C.said(e))
                    continue
                except RuntimeError as e:  # GitHub said no: the same answer goes again next pass
                    tries[name] = tries.get(name, 0) + 1
                    log(f"{tag}: couldn't post {V.NAMES[r]}'s review of {back['head'][:7]} ({tries[name]}): {e}")
                    # Three tries, and then only until the reviewer finds this
                    # head clean, which covers what it found there; one out of
                    # service may never, so the owner hears.
                    if tries[name] < 3 or said[r][0] != "clean":
                        x = f"{V.NAMES[r]}'s review of {back['head'][:7]}"
                        unposted.append(x)
                        if tries[name] >= 3:
                            digest.append(("A review couldn't be posted", line, f"{x}, {tries[name]} tries"))
                        strike("post", C.said(e))
                        continue
            elif (back.get("result") == "review" and isinstance(back.get("base"), str) and isinstance(back.get("head"), str)
                  and isinstance(back.get("answer"), dict) and back["answer"].get("findings")):
                # Findings on another base, which the PR left while the look
                # was under way: not posted here, but owed if it goes back.
                lost_on.setdefault(r, {})[back["base"]] = back["head"]
            tries.pop(name, None)
            V.drop(name)
        if tries:
            rs["late"] = tries
        else:
            rs.pop("late", None)

    for r in names:
        outs(r)

    def reading(rs):
        """Those of `rs` with a look at the PR, at any head, still under way:
        out of service, their verdict on this head doesn't cover it, so the
        pass waits for what it finds. One not begun never will be. Asked
        before this pass reads what came back, so one that ends meanwhile
        is read."""
        return [r for r in rs if not DRY and said[r][0] != "clean" and V.under_way(key, r, pr["baseRefName"])]

    was_out = [r for r in names if V.out(r)]
    under_way = reading(was_out)
    for r in names:
        # One out of service too, or put out by one of these answers: its
        # looks came back before it went out, and posting them asks nothing
        # of it.
        consume(r)
    for r in names:
        overtaken(r)
    # Put out by an answer this pass read: its others.
    under_way += reading([r for r in names if V.out(r) and r not in was_out])
    live = [r for r in live if not V.out(r)]
    for r in names:
        if V.out(r):
            # Back in service then: its look at this head is asked for.
            C.wake(repo, load(V.OUT, {})[r]["until"] + 1)  # ends: once; listed while it's out
    # Every reviewer's rounds, in service or not: one going out doesn't take
    # a PR back under the limit.
    rounds = max((said[r][2] for r in names), default=0)
    clean = bool(live) and all(said[r][0] == "clean" for r in live)
    # Past the round limit, the reviewers aren't asked again: every look so
    # far has found more, so more looks only drip-feed more rounds. A Claude
    # job settles what's left on its merits, fixing what's real and answering
    # the rest, and a judge decides whether it lands. Only what the judge
    # can't call (a one-way door) waits for the owner.
    capped = rounds >= rc["rounds"] and not clean

    def owed(r):
        """Past the limit, what the reviewer still owes the PR, which the
        judge mustn't land past: a look at any of its heads on this base
        still in the queue (begun or not, it's waited for), an answer back
        but not yet posted, or findings that couldn't be posted, at this
        head or an earlier one (it's asked again on this head: that's the
        same round, not another)."""
        base = pr["baseRefName"]
        return bool(V.in_queue(key, r, base) or V.answered(key, r) or base in lost_on.get(r, {}))
    # Findings on this head this pass posted are threads it never read: one
    # out now isn't waited for, and past the limit no look follows to cover
    # them, so they hold the pass back.
    late += [head[:7] for r in found if r not in live or capped]

    # 1. The reviewers.
    quiet = age >= rc["settleDelay"] * MIN
    looking = [r for r in live if owed(r)] if capped else [r for r in live if said[r][0] == "none"]
    if not quiet and looking:
        after(st["seen"], rc["settleDelay"])  # ends: the reviewers are asked then, once a head
    waiting = [f"Posting {x}" for x in unposted]
    for r in names:
        if r in live:
            continue
        if lost_on.get(r, {}).get(pr["baseRefName"]) == head:
            waiting.append(f"{V.NAMES[r]}'s findings on {head[:7]} couldn't be posted, and it's out of service")
        elif capped and owed(r) and r not in under_way:
            waiting.append(f"{V.NAMES[r]} still owes it a review, and it's out of service")
    waiting += [f"{V.NAMES[r]} is still reviewing it, though out of service" for r in under_way]
    if not live and not capped:
        why = "; ".join(f"{V.NAMES[r]}: {away[r] or V.out(r)}" for r in names) or "none is on"
        waiting.append(f"No reviewer can review it now ({why})")
        digest.append(("No reviewer can review it", line, why))
    for r in looking:
        if quiet and (not capped or pr["baseRefName"] in lost_on.get(r, {})):
            V.ask(repo, pr, key, r, st["rv"].setdefault(r, {}), after)
        waiting.append(f"{V.NAMES[r]} is reviewing this commit" if st["rv"].get(r, {}).get("queued")
                       else f"Waiting for {V.NAMES[r]}'s review" + ("" if capped else " of this commit"))
    # A look that can't happen, a first one or a second: the owner hears either way.
    stuck = []
    for r in live:
        if st["rv"].get(r, {}).get("fails", 0) >= 3:
            x = f"failed {st['rv'][r]['fails']} times on {head[:7]}"
            stuck.append(f"{V.NAMES[r]} {x}")
            digest.append((f"{V.NAMES[r]} couldn't review it", line, x + " (journalctl -u autoland-review)"))

    what = []
    if open_threads:
        what.append(f"{open_threads} open review thread{'s' * (open_threads > 1)}")
    if changes:
        what.append("changes requested by " + ", ".join(changes))
    if late:
        what.append("review findings on " + ", ".join(sorted(set(late))))
    if waiting:
        # One reviewer's threads needn't wait for the other to finish.
        if what:
            hand_off(", ".join(what), "; ".join(waiting + stuck + what), ours=not swept)
        else:
            settle("pending", "; ".join(waiting + stuck))
    elif what and capped:
        hand_off(f"{', '.join(what)}, after {rounds} review rounds with findings, the limit, so its reviewers won't look again. "
                 "Weigh each finding on its merits: fix what's a real defect and answer the rest with why. "
                 "A Claude job judges it after, without the reviewers",
                 f"Past the review-round limit ({rounds}): " + ", ".join(what), ours=not swept)
    elif (not clean and not capped) or what:
        if not what:
            # Every finding answered without a new commit: the reviewers that
            # had findings look again.
            for r in live:
                if said[r][0] == "findings":
                    after(said[r][1], rc["settleDelay"])  # ends: the look it asks for, once
                    if C.NOW - said[r][1] > rc["settleDelay"] * MIN:
                        V.ask(repo, pr, key, r, st["rv"].setdefault(r, {}), after)
            settle("pending", "Findings answered; " + ("; ".join(stuck) if stuck else f"review round {rounds + 1} next"))
        else:
            hand_off(", ".join(what), f"Round {rounds}: " + ", ".join(what), ours=not swept)
    # 2. The reviewers are clean on this head and nothing is open: CI, then land.
    elif ci == "red":
        # Its own once a head, apart from a findings job's: one that answered
        # a finding here without pushing didn't fix a check that failed since.
        hand_off("CI is red (" + ", ".join(ci_names) + ")", "CI is red: " + ", ".join(ci_names)[:100],
                 "redFixed", ours=not swept)
    elif held:
        judged("a Claude job was asked to judge this head", PAST_LIMIT if capped else REVIEWED)
    elif proof_only:
        prove()
    elif ci == "pending":
        settle("pending", "Settled; waiting for CI: " + ", ".join(ci_names)[:90])
    elif gate or unseen or ci == "none" or asked or capped:
        judged(f"it touches {gate}" if gate else
               f"it changes {', '.join(unseen[:5])}{' and more' * (len(unseen) > 5)}, which the reviewers aren't shown" if unseen
               else "the repo has no CI" if ci == "none" else "a Claude job was asked to judge this head" if asked
               else f"it went past the review-round limit ({rounds} rounds with findings)",
               PAST_LIMIT if capped else REVIEWED)
    else:
        land_now(f"{' and '.join(V.NAMES[r] for r in live)} found nothing on this commit, every thread is resolved, CI is green")

    open_a_day(our_status(pr)[1] or "unsettled")
    save(key, st)
