"""One PR's next step, for autoland-steward's pass (steward.py): where it
stands (its reviewers' verdicts, CI, its threads, its labels, what GitHub
says of its merge), and the thing to do now: queue a review, post what came
back, hand what's open (findings, red CI, a conflict with its base) to a
Claude job, have a job run its machine checks or judge it, post its
`review-settled` status and merge it (or, in a repo with its merge train
on, put it in the train, which lands it: train.py), or list it for the
owner or the stuck issue. Each step it takes is in the PR's status, in
what the watchdog watches (waits.py), and when it waits on something one of
the steward's labels names, in that label (prs.py).

A step that fails (GitHub refuses it, or no job can start) is tried again,
later each time, and never quietly for ever: the third failure in a row on a
head makes the PR's wait overdue, so the watchdog takes it (strike). A Claude
job that stops before it finishes (it times out, goes quiet or crashes) is
the machine failing, not the job's answer: it's started again the same way
(retry_at)."""

import re
import subprocess
import time

import common as C
import lock_wait
import proofs as P
import review_step
import reviewers as V
import stopping
import train as T
import waits
from common import CFG, DEP_RE, DRY, MIN, load, log, repo_cfg, save, ts
from seal import MAJOR as MAJOR_VERDICT  # renovate-sweep's, on a Renovate major
from touches import automerges, bot_edited, dep_kind, gate_edit, moved_from, one_way
from tasks import DEP_SEEN, PAST_LIMIT, REVIEWED, fix_task, judge_task, judged_on, judgment, owed, proof_task, red_fix
from prs import MERGE_STEPS, PROOF_SLOT, RELEASE_AGE, REVIEWER, ci_state, eyes, land, onto, our_status, post_status, show_waiting

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
    "empty": "what its merge would change can't be worked out (git can't fetch it or merge it)",
    "close": "GitHub keeps refusing to close it, though its base already has everything it changes",
    "mergeable": "GitHub won't say it can be merged",
    "job": "no Claude job can start for it",
    "build": "its proof build can't start (systemd-run refuses it)",
    "eval": "what its proof builds can't be worked out (its eval fails or times out)",
    "post": "GitHub keeps refusing a review it's to post",
    "lost": "its Claude jobs keep stopping before they finish (timed out, quiet or crashed)",
    "stop": "proj keeps refusing to stop its Claude jobs on a head it no longer has",
}
STRIKE_LIMIT = 3  # failures in a row of one step, on one head, that make its wait overdue


def clock(t):
    """A time in a PR's status, in the machine's own time zone: 6:47pm PDT."""
    return time.strftime("%-I:%M%p %Z", time.localtime(t)).replace("AM", "am").replace("PM", "pm")


def handle(repo, pr, repo_info, digest, held=None):
    rc = repo_cfg(repo)
    n, head = pr["number"], pr["headRefOid"]
    by_train = T.rides(repo, pr, repo_info)
    # A judge's verdict (and its job) is for this head on this base, with this
    # description (judged_on): one retargeted or redescribed is judged again.
    judged_at = f"{head} {judged_on(pr)}"
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
        stopping.moved(repo, pr, st)  # its jobs on the last head, to be stopped below
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

    # The jobs a new head left answering for nothing, stopped: one proj
    # refuses is tried again.
    if stopping.stop(tag, st):
        cleared("stop")
    else:
        strike("stop")

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

    refresh = {}  # what the watchdog's to count of a refresh of it renovate-sweep has (waits.note)

    def settle(state, desc, why=None, wait=waits.STEWARD, label=None):
        """The PR's status, what it waits on for the watchdog (waits.py), and
        its waiting label: label's (prs.py), or none. False if GitHub didn't
        take the status."""
        ok = post_status(repo, pr, state, desc)
        show_waiting(repo, pr, labels, label)
        if why:
            digest.append((why, line, desc))
        waits.note(repo, pr, desc, wait, **refresh)
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

    def update_branch(proof=False):
        """Bring a settled head up to date with a base whose rules want it
        (the merge won't), or whose builds its proof wants (proof: prove saw
        it behind): the merged-in head is reviewed like any other.
        GitHub makes it a moment later, and its push kicks a pass; one that
        hasn't come ten minutes on is asked for again. True while it's being
        brought up to date, False if it isn't behind, None if GitHub didn't
        take the ask (strike)."""
        if merging != "update" and not proof:
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
        at jobTimeout, a proof build stops at proofTimeout), and every
        quarter hour past that while it still hasn't (it waited for a slot
        first): its end kicks one, and this is the one that sees it if that
        kick never got through."""
        limit = CFG["proofTimeout"] if str(st.get(kind + "Job")).startswith(C.BUILD) else CFG["jobTimeout"]
        due = st.get(kind + "At", C.NOW) + (limit + 5) * MIN
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

    def started(kind, task, at=head, broken=False, begin=None):
        """A Claude job, once per head (at: once per head on its base, for
        the judge's; recorded once it has started; until then it's tried
        again (strike), and its caller lists it on the stuck issue), or
        what begin starts in its place and gives the id of (a proof build).
        One that isn't in place of one that broke (retry_at) ends a run of
        those."""
        jid = begin() if begin else C.job(repo, task, since=ts(pr["createdAt"]))
        step = "build" if begin else "job"
        if jid:
            st[kind] = at
            st[kind + "Job"] = jid
            st[kind + "At"] = C.NOW
            deadline(kind)
            cleared(step)
            if not broken:
                cleared("lost")
            log(f"{tag}: {'a build' if begin else 'a Claude job'} {jid} ({kind})")
        else:
            log(f"{tag}: couldn't start {'its build' if begin else 'a Claude job (no checkout, or proj refused)'}")
            strike(step)
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

    def again(kind, task, doing, answer, at=head, begin=None):
        """A job that ended without its answer is started once more; one that
        does that twice fails the PR's status and is listed for the owner.
        One that broke is started again once its retry is due, however many
        times, and doesn't count toward the two (retry_at). begin: a proof
        build's, as started has it (doing is then what it builds)."""
        how = broke(kind)
        last = f"build of {doing}" if begin else f"Claude job {doing}"
        if how and (due := retry_at(kind)) > C.NOW:
            settle("pending", f"Settled; the last {last} {how}, so another starts at {clock(due)}"[:140])
        elif not how and st.get(kind + "Again") == at:
            settle("failure", f"Settled, but two Claude jobs ended without {answer}"[:140],
                   f"Two Claude jobs ended without {answer}")
        elif started(kind, task, at, bool(how), begin):
            if not how:
                st[kind + "Again"] = at
            st.pop(kind + "Lost", None)
            busy = f"building {doing} here" if begin else at_work(kind, doing, at)
            settle("pending", f"Settled; {busy} again (the last one {how or f'ended without {answer}'})"[:140])
        elif begin:
            settle("pending", f"Settled, but the build of {doing} couldn't start again"[:140], "Couldn't start a proof build")
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
        ours = ours and not requested  # Renovate would redo its branch
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
            # A conflict's merge is a new head, whose proofs run again anyway:
            # its job doesn't rerun them, which can outlast it like the gate.
            conflict = kind == "conflictFixed"
            proofs = {c: machine[c] for c in ci_names if c in machine} if ci == "red" and not conflict else {}
            train = ci == "red" and any(c.startswith(f"{T.CONTEXT} (") for c in ci_names)
            if started(kind, fix_task(repo, pr, why, proofs, train, conflict), at, bool(how)):
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

    def already_landed(tip):
        """Its merge would change nothing on its base, at tip (onto):
        taken back from anything that would merge it, and closed, saying so.
        A dependency bot's is left to its bot, which closes its own once it
        sees there's nothing left to update: one closed under it is an update
        it would ignore from then on."""
        said = f"{pr['baseRefName']} at {tip[:7]} already has everything it changes"
        if dep:
            settle("pending", f"Already landed: {said}; its bot closes it"[:140])
            return
        settle("pending", f"Already landed: {said}; closing it"[:140])
        if C.write("pr", "close", str(n), "-R", repo, "--comment",
                   f"Closed without merging: {said}, so merging this would only add an empty commit. "
                   "What it does landed another way, most likely through another PR.", check=False) is None:
            strike("close")
            return
        cleared("close")
        log(f"{tag}: {said}; closed it")

    def mergeable():
        """Whether anything may merge it now, the steward, its train or its
        bot: it changes something on its base's tip as GitHub has it now
        (onto), and a fix for a red default branch only on the tip this pass
        saw red (R.watch_default holds it on any other word from CI). A tip
        that has moved on since (a merge earlier in this very pass, or
        anyone's push) has no word yet, so a fix waits for its CI. Otherwise
        it's taken back from anything that would merge it, saying why: one
        that changes nothing is closed (already_landed), and one whose merge
        git can't work out is tried again."""
        try:
            tip, nothing = onto(repo, pr)
        except (RuntimeError, OSError, subprocess.SubprocessError) as e:
            settle("pending", "Settled, but what its merge would change can't be worked out; trying again")
            strike("empty", str(e))
            return False
        cleared("empty")
        if nothing:
            already_landed(tip)
            return False
        default = repo_info["defaultBranchRef"]["name"]
        if tip and red_fix(pr, default) and tip != ((repo_info["defaultBranchRef"].get("target") or {}).get("oid")):
            settle("pending", f"Waiting for CI on {default}: it lands only while {default} is red, and it's moved on"[:140])
            return False
        return True

    def land_now(desc):
        """Settled: merged, unless what GitHub says of its merge asks for
        something first (MERGE_STEPS). Behind a base whose rules want it up
        to date, it's brought up to date instead, and the merged-in head is
        reviewed and its CI run like any other before it lands. (One that
        conflicts with its base never gets here: it's handed on first.) In a
        repo with its merge train on, one against the default branch goes in
        the train instead (train.py's ride). First of all, only one anything
        may merge now (mergeable)."""
        if not mergeable():
            return
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
                     "meant. If the two sides can't both be kept, which one stays "
                     f"is the owner's call. Just before you merge, and again before you push, check that the PR is still open "
                     f"on {base} at head {head}: if its base or head moved on, stop without merging or pushing, since the "
                     "steward hands what it is now to a new job", f"Conflicts with {base}", "conflictFixed", f"{head} {base}",
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
        judged_owner = judgment(pr, head, repo) == "owner"
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
        verdict = judgment(pr, head, repo)
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
            hand_off("the Claude job that judged it found something failing on this head, its build or tests, a finding "
                     "left unfixed or a reason in its description that doesn't hold (its comment on the PR says what)",
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
        """The reviewers are done, and checks that run on a machine, not in
        CI, are left. Those with a proofBuilds attribute go first, built by
        the steward itself (proofs.build), no Claude job, while CI still
        runs (build_early); the rest, once CI and the builds have passed, a
        Claude job runs on this head and records their verdicts. Either
        starts once its repo's proofSlots have room for it (one started
        again too), or it has nothing new to build; with nothing to build
        at all, neither: the verdicts are taken from the store (proofs.py)."""
        builds = [c for c in ci_names if c in rc["proofBuilds"]]
        beside = [c for c in ci_names if c not in machine]  # CI still running
        checks = builds or ci_names
        names = ", ".join(checks)
        if builds:
            doing, task = names, None

            def begin():
                return P.build(repo, pr, checks, st)
        else:
            doing, task, begin = f"running {names}", proof_task(repo, pr, checks, rc["proofJobs"]), None

        def running_now():
            skipped = "; nothing new to build, so it skipped the line" if st.get("provedFree") == head else ""
            busy = f"building {names} here" if builds else at_work("proved", doing)
            ci_too = f", CI ({', '.join(beside)}) beside it" if beside else ""
            settle("pending", f"Settled; {busy}{ci_too}{skipped}"[:140])

        # What this proof is: a build or a Claude job, of which checks, each
        # built from which attribute. This head's proof so far was another
        # (proofs.fits: a build before the Claude jobs, a job from before
        # its check had a build, a check added or remapped since): this one
        # starts afresh, nothing of that one's carried over (its retries,
        # the two lost answers, its verdicts). One still at work is stopped,
        # on the stopping path when proj or systemd won't just now, and
        # holds its slot until it's over (proofs.live).
        stage = ["build" if builds else "job", sorted(checks), {c: rc["proofBuilds"].get(c) for c in checks}]
        if st.get("proved") == head and (jid := st.get("provedJob")) and not P.fits(st, stage):
            st["proofsLive"] = P.jobs(st)
            if running("proved") and not C.stop_job(jid):
                st["stopping"] = list(dict.fromkeys(st.get("stopping", []) + [jid]))
            for k in ("", "Job", "At", "Lost", "Again", "Broke", "For", "Unposted"):
                st.pop("proved" + k, None)
            ends.pop("proved", None)
        # A build of this head that's done: the verdicts it kept. A failure's
        # log goes on the PR first, so a comment GitHub didn't take is tried
        # again before the red status hands the PR to a fix; verdicts GitHub
        # took only some of are posted again even once it's red (handle).
        if builds and st.get("proved") == head and not running("proved") and ends.get("proved") == "done":
            jid = st["provedJob"]
            said = P.verdicts(jid)
            if said is not None:
                # Only what's left to post: the ones it posted are green.
                said = {c: v for c, v in said.items() if c in checks}
            if said is None:
                # One that kept no verdicts broke: it's built again (below).
                ends["proved"] = "failed"
            else:
                failed = [c for c, (state, _) in said.items() if state == "failure"]
                if failed and st.get("provedSaid") != jid:
                    if C.comment(repo, n, P.failed_log(jid, failed)) is not None:
                        st["provedSaid"] = jid
                said_all = not failed or st.get("provedSaid") == jid
                if said_all and P.record(repo, pr, said):
                    st.pop("provedUnposted", None)
                    # Their statuses kick the pass that lands it, or hands it to a fix.
                    settle("pending", f"Settled; {names} built here, its verdict posted"[:140])
                    return
                if said_all:
                    st["provedUnposted"] = jid
                strike("status")
                settle("pending", f"Settled; {names} built here, its verdict not posted yet"[:140])
                return
        retry = lost("proved")
        if not retry and st.get("proved") == head:
            running_now()
            return
        if (drvs := P.built(repo, pr, checks, st)) and not P.record(repo, pr, P.stored(checks, drvs)):
            strike("status")
        if drvs:  # Their statuses kick the pass that lands it.
            settle("pending", f"Settled; {names} taken from the store, not run again"[:140])
            return
        # One that broke is counted, and its retry booked, whether or not
        # there's room: only the job started in its place waits for a slot.
        if retry and broke("proved"):
            retry_at("proved")
        slots = rc.get("proofSlots")
        outs = P.outputs(repo, pr, checks, st) if slots or builds else None
        if builds and outs is None:
            # Nothing to build without its derivations: tried again on a
            # later pass (outputs).
            strike("eval")
            settle("pending", f"Settled, but what {names} builds can't be worked out here yet"[:140])
            return
        if builds:
            cleared("eval")
        # Behind its base (the default branch, whose tip this pass read),
        # with what it builds merged in all built already: brought up to
        # date, its new head's verdicts come from the store. Not a
        # dependency update's: its bot keeps that branch.
        default = repo_info.get("defaultBranchRef") or {}
        tip = (default.get("target") or {}).get("oid") if pr["baseRefName"] == default.get("name") else None
        if (builds and not retry and not DEP_RE.match(pr["headRefName"])
                and (up := P.merged(repo, pr, checks, st, tip)) and not P.unbuilt(up)):
            if update_branch(proof=True):
                settle("pending", f"Settled; brought up to date with {pr['baseRefName']}, which has {names} built here already"[:140])
                return
            settle("pending", "Settled; GitHub didn't take its update with its base, so trying again"[:140])
            return
        free = False
        # One whose next step is failing the PR (again) needs no slot.
        last = retry and st.get("provedAgain") == head and not broke("proved")
        if slots and not last and (ahead := P.ahead(repo, n, st, slots, outs)) is not None:
            if ahead != P.FREE:
                P.wait(repo, n)
                settle("pending", f"Settled; waiting its turn to {'build' if builds else 'run'} {names} ({ahead} ahead)"[:140],
                       label=PROOF_SLOT)
                return
            free = True
        before = st.get("provedJob")
        st["provedFree"] = head if free else None
        if retry:
            again("proved", task, doing, "its verdict", begin=begin)
        elif started("proved", task, begin=begin):
            running_now()
        elif builds:
            settle("pending", f"Settled, but the build of {names} couldn't start"[:140], "Couldn't start a proof build")
        else:
            settle("pending", f"Settled, but no Claude job could run {names}"[:140],
                   "Couldn't start a Claude job to run its checks")
        if st.get("provedJob") != before:
            st["provedFor"] = stage
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
    if held:
        # A fix for a red default branch that mustn't land now
        # (R.watch_default): the branch isn't red any more, or its CI hasn't
        # said yet whether it still is. Like a hold, nothing may merge it.
        settle("pending", held[:140])
        save(key, st)
        return
    if CFG["labels"]["hold"] in labels:
        # Held after it was settled: take back the status and the queued merge.
        log(f"{tag}: on hold")
        settle("pending", f"On hold (the {CFG['labels']['hold']} label)", wait=waits.HOLD)
        save(key, st)
        return

    machine = C.machine(rc)  # the checks a machine runs, not CI, and how
    ci, ci_names, docs = ci_state(pr, machine, moved := moved_from(repo, pr, st), repo)
    # A build's verdicts GitHub took only some of: the rest go up whatever
    # CI says now, so a failure it took doesn't leave the others unposted
    # behind a red PR. Their statuses kick the pass that sees them. Only
    # those of its checks still built from the attribute it built
    # (proofs.still): one dropped or remapped since waits for its own build.
    if (left := st.get("provedUnposted")) and st.get("proved") == head:
        said = P.still(st, P.verdicts(left) or {}, rc["proofBuilds"])
        if not said or P.record(repo, pr, said):
            st.pop("provedUnposted")
        else:
            strike("status")
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
    gate = one_way(pr, rc, moved)
    renovate = pr["headRefName"].startswith("renovate/")
    proof_only = ci == "pending" and bool(ci_names) and all(c in machine for c in ci_names)
    # A check the steward builds itself costs no Claude job, so it needn't
    # wait for CI to pass: it starts once the reviewers are done, and builds
    # while CI runs rather than after it. A Claude job's check still waits.
    build_early = ci == "pending" and any(c in rc["proofBuilds"] for c in ci_names)
    # A minor dependency PR a Claude job fixed carries Claude's code, which no
    # reviewer has read (and Renovate won't merge); a bot's PR that changes a
    # workflow beyond repinning its actions changes the gate. Either goes
    # through the review loop below like a Claude PR. While a Renovate one's CI
    # is red it's renovate-sweep's to hand over, findings and all: a job from
    # each on one branch would push over each other. The sweep's push makes a
    # new head, and the reviewers read it again.
    # A judge asked for on this head and base (the repo had no CI then, say)
    # still decides once CI shows up, or the description is edited (then on
    # the new one): its owner or failed verdict isn't dropped for either.
    judge_said = judgment(pr, head, repo)
    asked = owed(pr, head, repo, st.get("judged"))
    # Its owner or failed verdict holds it whatever CI is still doing; only
    # a land waits for CI.
    held = asked and judge_said in ("owner", "failed")
    fixed = dep == "minor" and not gate and (bot_edited(repo, pr, st) or gate_edit(repo, pr, st))
    swept = fixed and ci == "red" and renovate
    # The date a lock file refresh waits for, renovate-sweep's to have it refreshed then.
    requested = renovate and lock_wait.requested(pr, C.NOW, CFG["renovateRun"] or 0)
    refresh = lock_wait.refresh(pr, ci == "red", ci_names, C.renovated, requested) if renovate else {}

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
        if (ripe := refresh.get("ripe")) and not threads:
            when = time.strftime("%b %-d, ", time.localtime(ripe)) + clock(ripe)
            settle("pending", f"Lock file refresh, nothing in it old enough until {when}; renovate-sweep has it refreshed then",
                   wait=waits.SWEEP, label=RELEASE_AGE)
        elif renovate and ci == "red":
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
        elif proof_only or build_early:
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
        elif renovate and not T.on(repo):
            # Renovate merges its own, on its next run on the repo, except in
            # a repo with its train on, where it merges none (updates.nix):
            # the train lands those against the default branch, and land_now
            # the rest. One with automerge on that's still open
            # past renovateMergeAfter is a stall, which that run is kicked for
            # first (waits.py). Not one nothing may merge (mergeable): one
            # whose change has landed already is Renovate's to close.
            if mergeable():
                settle("success", "Dependency update, CI green", wait=waits.RENOVATE if automerges(pr) else waits.STEWARD)
        else:
            land_now("Dependency update, CI green")
        open_a_day(f"Dependency update, CI {ci}", swept=renovate)
        save(key, st)
        return

    # The reviewers (review_step.py).
    live, said, rounds, clean, capped, unseen, late, waiting, stuck, out_of_service = review_step.look(
        repo, pr, key, st, lost_on, digest, line, age, after, strike, cleared)

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
            settle("pending", "; ".join(waiting + stuck), label=REVIEWER if out_of_service else None)
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
    elif proof_only or build_early:
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
        land_now(f"{' and '.join(V.NAMES[r] for r in live)} found nothing on this commit, every thread is resolved, CI is green{' (docs only)' * docs}")

    open_a_day(our_status(pr)[1] or "unsettled")
    save(key, st)
