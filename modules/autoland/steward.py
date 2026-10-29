"""autoland-steward: every PR in every repo the owner has, from ready to merged.

A full pass (burrow-events runs one when it starts, and hourly, for what no
event reports):

  1. Find the repos: every non-archived, non-fork repo of each owner in
     `owners`, new ones included. Bring each one's settings to the baseline
     (auto-merge, delete-branch-on-merge, squash, secret scanning, the
     `baseline` ruleset); a repo seen for the first time also gets a Claude
     job that brings its files to docs/repo-baseline.md (repos.py).
  2. For each open PR that isn't a draft, work out where it stands and do
     the next thing: queue a review of its head from each reviewer (Codex,
     reviewers.py), post what they found, hand open findings
     or red CI to a Claude job,
     have a Claude job run the checks only a machine of the owner's runs
     (proofJobs) or judge a PR it won't land on its own word (tasks.py says
     what each job is told), post the `review-settled` commit status and
     merge it, or list it for the owner.
  3. Once a day per repo, delete branches whose tip is already in the
     default branch, and list the stale unmerged ones.
  4. Read each repo's Renovate dashboard: what sits on it too long, short
     of a PR, goes to a Claude job (dashboard.py); and its quarantined
     flaky tests, each to a Claude job that fixes it (flakes.py).
  5. Keep one issue up to date with everything waiting for the owner, and
     one with what the pipeline is stuck on, for a Claude job (repos.py).

`autoland-steward kick OWNER/REPO` is a pass over that repo alone (2, 4
and 5), the one burrow-events starts within seconds of what changes it:
a webhook delivery, a Claude job's turn ending, a reviewer's answer coming
back, or a wait the last pass booked ending (common.wake).

Everything talks to GitHub as the steward's own GitHub App (common.py), so
its statuses, merges and the reviewers' reviews are posted as the App, and
nobody else's count as its word. Its Claude jobs work as the machine user.
Settings are in the JSON file $AUTOLAND_CONFIG (../autoland.nix). State is
in config.stateDir: one JSON file per PR, one per repo.

`autoland-steward status` prints what it thinks of every open PR without
changing anything.

`autoland-steward review` works through the reviews the passes queued
(review_worker.py), as the reviewers' own user. A careful look takes an
agent many minutes, so it runs as its own service and no pass waits for it.
"""

import fcntl
import os
import re
import sys
import time

import common as C
import dashboard as D
import flakes as F
import proofs as P
import repos as R
import reviewers as V
import waits
from common import CFG, DRY, MIN, STATE, load, log, repo_cfg, save, ts
from seal import MAJOR as MAJOR_VERDICT  # renovate-sweep's, on a Renovate major
from touches import bot_edited, dep_kind, gate_edit, moved_from, one_way
from tasks import DEP_SEEN, JUDGMENT, PAST_LIMIT, REVIEWED, fix_task, judge_task, proof_task
from prs import ci_state, eyes, land, open_prs, our_status, post_status


# The link Claude Code puts in a PR its session opens.
SESSION = re.compile(r"https://claude\.ai/code/session_\w+")
# What a fix job of each kind is on, for a PR's status while another waits.
ON = {"handed": "its findings", "redFixed": "its red CI", "failFixed": "what its judge found failing",
      "conflictFixed": "its conflicts with its base"}


def clock(t):
    """A time in a PR's status, in the machine's own time zone: 6:47pm PDT."""
    return time.strftime("%-I:%M%p %Z", time.localtime(t)).replace("AM", "am").replace("PM", "pm")


def handle(repo, pr, repo_info, digest, obsolete=False):
    rc = repo_cfg(repo)
    n, head = pr["number"], pr["headRefOid"]
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
        st.update(head=head, seen=C.NOW, handed=None, landFails=0, rv={})
    st.setdefault("rv", {})
    labels = {x["name"] for x in pr["labels"]["nodes"]}
    age = C.NOW - st["seen"]
    tag = f"{repo}#{n}"
    line = f"[{repo}#{n}]({pr['url']}) {pr['title']}"

    def after(t, minutes):
        """A wait that ends `minutes` after t: this repo gets a pass then."""
        C.wake(repo, t + minutes * MIN + 1)

    # Open a day, it's listed for the owner (below): a pass then says so.
    after(ts(pr["createdAt"]), 24 * 60)

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
        after(since, wait)
        return since + wait * MIN

    def settle(state, desc, why=None, wait=waits.STEWARD):
        """The PR's status, and what it waits on for the watchdog (waits.py)."""
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
        if not ok and not DRY:
            # GitHub didn't take the status or the stop: until it does, the
            # old ones stand and a merge queued on it can still go. Again in a minute.
            after(C.NOW, 1)

    def update_branch():
        """Bring a settled head up to date with a base whose rules want it
        (the merge won't): the merged-in head is reviewed like any other.
        GitHub makes it a moment later, and its push kicks a pass; one that
        hasn't come ten minutes on is asked for again. True while it's being
        brought up to date, False if it isn't behind, None if GitHub didn't
        take the ask (again in a minute)."""
        if pr.get("mergeStateStatus") != "BEHIND":
            return False
        if st.get("updated") == head and C.NOW - st.get("updatedAt", 0) < 10 * MIN:
            after(st["updatedAt"], 10)
            return True
        if C.write("api", "-X", "PUT", f"repos/{repo}/pulls/{n}/update-branch",
                   "-f", f"expected_head_sha={head}", check=False) is None:
            after(C.NOW, 1)
            return None
        log(f"{tag}: behind its base, brought up to date")
        st["updated"], st["updatedAt"] = head, C.NOW
        after(C.NOW, 10)
        return True

    def conflicted():
        """Settled, but its changes conflict with its base's: GitHub won't
        merge it or bring it up to date, so a Claude job merges the base in,
        once a head on its base. Its push is a new head, reviewed like any other. True while it does."""
        if pr.get("mergeStateStatus") != "DIRTY":
            return False
        base = pr["baseRefName"]
        hand_off(f"it conflicts with {base}, so GitHub won't merge it: merge {base} into the branch, resolve the "
                 "conflicts keeping what both sides meant, run the tests and push",
                 f"Settled, but it conflicts with {base}", "conflictFixed", judged_at, wait=False)
        return True

    def deadline(kind):
        """A pass when this kind's job is due to have ended (proj stops a turn
        at jobTimeout), and every quarter hour past that while it still
        hasn't (it waited for a slot first): its end kicks one, and this is
        the one that sees it if that kick never got through."""
        due = st.get(kind + "At", C.NOW) + (CFG["jobTimeout"] + 5) * MIN
        C.wake(repo, due if due > C.NOW else C.NOW + 15 * MIN)

    def running(kind, at=head):
        """This head's job of the kind (at: on its base, for the judge's and
        the fix it asks for) while it's still queued or running, or proj
        can't say: "queued", "running" or "unknown"; else "". One that is
        keeps its deadline, since each pass replaces the repo's wake-ups."""
        jid = st.get(kind + "Job")
        if st.get(kind) != at or not jid:
            return ""
        state = C.job_state(jid)
        if state not in (None, "queued", "running"):
            return ""
        deadline(kind)
        return state or "unknown"

    def started(kind, task, at=head):
        """A Claude job, once per head (at: once per head on its base, for
        the judge's; recorded once it has started; until then it's tried
        again every five minutes and listed for the owner)."""
        jid = C.job(repo, task)
        if jid:
            st[kind] = at
            st[kind + "Job"] = jid
            st[kind + "At"] = C.NOW
            deadline(kind)
            log(f"{tag}: a Claude job {jid} ({kind})")
        else:
            log(f"{tag}: couldn't start a Claude job (no checkout, or proj refused)")
            after(C.NOW, 5)
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
            after(C.NOW, 1)  # the second look
        return seen

    def again(kind, task, doing, answer, at=head):
        """A job that ended without its answer is started once more; one that
        does that twice fails the PR's status and is listed for the owner."""
        if st.get(kind + "Again") == at:
            settle("failure", f"Settled, but two Claude jobs ended without {answer}"[:140],
                   f"Two Claude jobs ended without {answer}")
        elif started(kind, task, at):
            st[kind + "Again"] = at
            st.pop(kind + "Lost", None)
            settle("pending", f"Settled; a Claude job is {doing} again (the last one ended without {answer})"[:140])
        else:
            settle("pending", f"Settled, but no Claude job could start {doing} again"[:140],
                   "Couldn't start a Claude job")

    def fixing():
        """The kind of fix job handed on this head that's still queued or
        running (or that proj can't say about), else None."""
        for k in ON:
            if running(k, judged_at if k in ("failFixed", "conflictFixed") else head):
                return k
        return None

    def hand_off(why, desc, kind="handed", at=head, wait=True, ours=True):
        """Pending on desc while a Claude job fixes why, one of each kind a
        head (at), and the status says who has it: that job and since when,
        the Claude session that opened the PR and until when, or when a job
        takes it. A job does once it has sat long enough (handoff_at), or at
        once if it isn't to wait; never if it isn't ours to hand (a Renovate
        update renovate-sweep has). One that ended with this still to fix (it
        crashed, timed out, went quiet or pushed nothing) is handed once
        more; after a second the status fails and the owner hears, until a
        new head. Never while another fix job is still working on this head:
        two on one branch push over each other."""
        retry = st.get(kind) == at
        state = retry and running(kind, at)
        gone = retry and not state and lost(kind, at)
        if gone and st.get(kind + "Again") == at:
            settle("failure", f"Two Claude jobs ended without fixing it: {desc}"[:140],
                   "Two Claude jobs ended without fixing it")
            return
        if state:
            who = f"a Claude job {'is queued for it' if state == 'queued' else 'has it'} since {clock(st[kind + 'At'])}"
        elif retry and not gone:
            who = "its Claude job just ended"  # the second look is booked
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
            if started(kind, fix_task(repo, pr, why, proofs), at):
                if retry:
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
        # is tried again next pass, and the owner hears after three.
        if land(repo, pr, repo_info):
            st["landed"] = head
            return
        st["landFails"] = st.get("landFails", 0) + 1
        log(f"{tag}: merge failed ({st['landFails']})")
        after(C.NOW, 1)
        if st["landFails"] >= 3:
            digest.append(("Couldn't merge", line, "GitHub keeps refusing the merge (journalctl -u autoland)"))

    def land_now(desc):
        # Behind a base whose rules want it up to date, GitHub refuses the
        # merge until it is: brought up to date instead, and the merged-in
        # head is reviewed and its CI run like any other before it lands.
        if conflicted():
            return
        behind = update_branch()
        if behind is not False:
            why = "brought up to date" if behind else "GitHub didn't take the update, again in a minute"
            settle("pending", f"Settled, but behind its base: {why}")
            return
        settle("success", desc)
        # Still open with a merge queued on it: GitHub's to merge later, which
        # land never leaves it to (a steward before it did). Merged now instead.
        if st.get("landed") == head and C.queued(pr):
            st["landed"] = None
        if st.get("landed") != head:
            log(f"{tag}: settled, landing")
            merge()

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
        elif conflicted():
            pass  # judged once its merge of the base is a new head
        elif update_branch():
            settle("pending", "Settled, but behind its base: brought up to date, reviewed again")
        elif lost("judged", judged_at):
            again("judged", judge_task(repo, pr, why, ci == "none", seen), "judging it", "a verdict", judged_at)
        elif st.get("judged") == judged_at or started("judged", judge_task(repo, pr, why, ci == "none", seen), judged_at):
            settle("pending", f"Settled; a Claude job is judging it ({why})")
        else:
            settle("pending", f"Settled, but no Claude job could judge it ({why})", "Couldn't start a Claude job to judge it")

    def prove():
        """Only checks that run on a machine, not in CI, are left: a Claude
        job runs them on this head and records their verdicts, once its
        repo's proofSlots have room for it (a job started again too)."""
        doing = "running " + ", ".join(ci_names)
        task = proof_task(repo, pr, ci_names, rc["proofJobs"])
        retry = lost("proved")
        if not retry and st.get("proved") == head:
            settle("pending", f"Settled; a Claude job is {doing}"[:140])
            return
        slots = rc.get("proofSlots")
        if slots and not (retry and st.get("provedAgain") == head) and (ahead := P.ahead(repo, n, st, slots)) is not None:
            P.wait(repo, n)
            settle("pending", f"Settled; waiting its turn to run {', '.join(ci_names)} ({ahead} ahead)"[:140])
            return
        if retry:
            again("proved", task, doing, "its verdict")
        elif started("proved", task):
            settle("pending", f"Settled; a Claude job is {doing}"[:140])
        else:
            settle("pending", f"Settled, but no Claude job could run {', '.join(ci_names)}"[:140],
                   "Couldn't start a Claude job to run its checks")
        P.started(st)

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
    # runs the build and tests itself.
    if ci == "none" and not rc["requireCi"]:
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
        elif renovate:
            # Renovate merges its own.
            settle("success", "Dependency update, CI green")
        else:
            land_now("Dependency update, CI green")
        # Renovate's stuck ones are renovate-sweep's; a Dependabot one that
        # still hasn't merged after a day is the owner's to look at.
        if (not renovate and C.NOW - ts(pr["createdAt"]) > 86400
                and not any(x[1] == line for x in digest)):
            digest.append(("Open over a day", line, f"Dependency update, CI {ci}"))
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
                    after(C.NOW, 1)
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
                        after(C.NOW, 1)
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
                after(C.NOW, 1)  # the second look

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
                except ValueError as e:  # an answer that's no review
                    log(f"{tag}: {V.NAMES[r]}'s review of {back['head'][:7]} wasn't a review: {e}")
                except C.Unavailable as e:  # GitHub didn't answer: no try spent, the answer's kept
                    log(f"{tag}: couldn't post {V.NAMES[r]}'s review of {back['head'][:7]}, GitHub's unavailable: {e}")
                    unposted.append(f"{V.NAMES[r]}'s review of {back['head'][:7]}")
                    after(C.NOW, 1)
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
                        after(C.NOW, 1)
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
            C.wake(repo, load(V.OUT, {})[r]["until"] + 1)
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

    def ask(r):
        """Queue the reviewer on this head, unless it's already on its way.
        Its looks at one head are bounded by `rounds`: each one with findings
        counts as a round."""
        rs = st["rv"].setdefault(r, {})
        if rs.get("queued"):
            return
        if DRY:
            log(f"  would: ask {V.NAMES[r]} to review {tag} at {head[:7]}")
            return
        if rs.get("fails", 0) >= 3 and C.NOW - rs.get("failedAt", 0) > 3600:
            rs["fails"] = 0  # an outage passes: three more tries an hour on
        if rs.get("fails", 0) >= 3:
            after(rs.get("failedAt", 0), 60)
            return
        if rs.get("fails") and C.NOW - rs["failedAt"] < 5 * MIN:
            # Tries five minutes apart, however many events come meanwhile:
            # a hiccup gets time to pass before all three are spent.
            after(rs["failedAt"], 5)
            return
        try:
            V.queue(repo, pr, key, r)
        except Exception as e:  # the snapshot: GitHub, git, the disk
            rs["fails"], rs["failedAt"] = rs.get("fails", 0) + 1, C.NOW
            log(f"{tag}: couldn't queue {V.NAMES[r]} on {head[:7]} ({rs['fails']}): {str(e)[:200]}")
            after(C.NOW, 5)
            return
        log(f"{tag}: queued a {V.NAMES[r]} review of {head[:7]}")
        rs["queued"] = C.NOW

    # 1. The reviewers.
    quiet = age >= rc["settleDelay"] * MIN
    looking = [r for r in live if owed(r)] if capped else [r for r in live if said[r][0] == "none"]
    if not quiet and looking:
        after(st["seen"], rc["settleDelay"])  # the reviewers are asked then
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
            ask(r)
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
                    after(said[r][1], rc["settleDelay"])
                    if C.NOW - said[r][1] > rc["settleDelay"] * MIN:
                        ask(r)
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

    # Anything not merged after a day, reviewed clean or not (a check that never
    # reports keeps it pending), unless it's already listed for the owner.
    if C.NOW - ts(pr["createdAt"]) > 86400 and not any(x[1] == line for x in digest):
        digest.append(("Open over a day", line, our_status(pr)[1] or "unsettled"))
    save(key, st)


# ---------------------------------------------------------------- main

def main():
    if sys.argv[1:] == ["review"]:
        import review_worker
        review_worker.work()
        return
    try:
        for d in ("", V.QUEUE, V.SNAP, V.DONE, V.SESSIONS, V.GIT):
            os.makedirs(os.path.join(STATE, d), exist_ok=True)
    except OSError:
        if not DRY:
            raise
    # `kick REPO...`: just those repos' PRs, now (burrow-events, on what
    # changed on GitHub or here, or a wait that has ended). The full pass,
    # when burrow-events starts and hourly, does the rest: the settings, new
    # repos' baseline jobs and branches.
    kick = [k.lower() for k in sys.argv[2:]] if sys.argv[1:2] == ["kick"] else None
    if not DRY:
        # One pass at a time, full or kicked: two would act on the same PRs.
        lock = open(os.path.join(STATE, "pass.lock"), "w")
        fcntl.flock(lock, fcntl.LOCK_EX)
    try:
        run_pass(kick)
    finally:
        if C.LIMITED and C.LIMITED[0] > C.NOW:
            C.after_limit(kick)


def run_pass(kick):
    waits.begin()
    repos = R.discover()
    if kick is not None:
        repos = [r for r in repos if r["repo"].lower() in kick]  # GitHub's names are case-blind
    # Repos whose baseline job has started. A repo not in it gets one, a few
    # per pass so the first pass on a machine doesn't start them all at once.
    known = set(load("known.json", []))
    # Ones whose job didn't start: tried again a day later, so they don't
    # take every pass's slots.
    failed = load("baseline-failed.json", {})
    started = 0
    # What waits for the owner, by repo: a kicked pass replaces its repos'.
    digest = {}
    read, live = set(), set()  # the repos whose PRs it read, and their heads' snapshots
    covered = []
    for r in repos:
        repo = r["repo"]
        digest[repo] = mine = []
        with C.acting_for(repo):
            try:
                new = kick is None and R.settle_repo(r, known)
                if (new and CFG["baselineJobs"] and started < CFG["baselinePerPass"]
                        and C.NOW - failed.get(repo, 0) > 86400):
                    jid = C.job(repo, CFG["baselineTask"].replace("REPO", repo), clone=True)
                    started += 1
                    if jid:
                        log(f"{repo}: baseline job {jid}")
                        known.add(repo)
                        failed.pop(repo, None)
                    else:
                        log(f"{repo}: baseline job didn't start (no checkout, or proj refused); trying again tomorrow")
                        failed[repo] = C.NOW
                elif new and not CFG["baselineJobs"]:
                    known.add(repo)
                owner, name = repo.split("/")
                info, prs = open_prs(owner, name)
                P.begin(repo, (pr["number"] for pr in prs))
                read.add(repo.replace("/", "_"))
                waits.read(repo, prs)
                # First, so a fix PR it made obsolete isn't merged by this same pass.
                obsolete = R.watch_default(r, info, prs, mine) or set()
                # Every open PR's branches, head and base, drafts and forks' PRs
                # included: the branch sweep leaves them all (deleting a base
                # closes the PRs on it).
                in_use = set()
                seen_to = True
                # Obsolete fixes first: their holds take back a queued merge
                # before anything else this pass waits on.
                # Then oldest first, so proofSlots go to PRs in their order.
                for pr in sorted(prs, key=lambda pr: (pr["number"] not in obsolete, pr["number"])):
                    in_use.update((pr["headRefName"], pr["baseRefName"]))
                    live.add(V.snap_name(f"pr-{repo.replace('/', '_')}-{pr['number']}.json", pr["headRefOid"], pr["baseRefName"]))
                    try:
                        handle(repo, pr, info, mine, pr["number"] in obsolete)
                    except Exception as e:  # one bad PR mustn't stop the rest
                        log(f"{repo}#{pr['number']}: {e}")
                        waits.failed(repo, pr)
                        seen_to = False
                if seen_to:
                    covered.append(r)
                else:
                    C.wake(repo, C.NOW + 5 * MIN)
                if CFG["cleanBranches"]:
                    # Stale ones the last sweep found are listed every pass.
                    R.sweep_branches(r, in_use, mine, sweep=kick is None)
            except Exception as e:
                log(f"{repo}: {e}")
                waits.failed(repo)
                C.wake(repo, C.NOW + 5 * MIN)  # GitHub, often: another look soon
            if CFG["dashboard"]["enable"]:
                try:
                    D.watch(repo, mine)
                except Exception as e:  # its own: the repo's PRs were seen to all the same
                    log(f"{repo}: its dashboard: {e}")
                    C.wake(repo, C.NOW + 5 * MIN)
            if CFG["flaky"]["enable"]:
                try:
                    F.watch(repo, mine)
                except Exception as e:  # likewise
                    log(f"{repo}: its flaky tests: {e}")
                    C.wake(repo, C.NOW + 5 * MIN)
    # This pass's wake-ups and owner's lines replace the ones it had for the
    # repos it saw; a full pass's, all of them.
    seen = {r["repo"] for r in repos}
    wakes = {k: v for k, v in load("wake.json", {}).items() if v > C.NOW and k not in seen}
    save("wake.json", {**wakes, **C.WAKE})
    if kick is not None:
        digest = {**{k: v for k, v in load("digest-repos.json", {}).items() if k not in seen}, **digest}
    save("digest-repos.json", digest)
    waits.publish(seen, digest, kick is None)
    if kick is None:
        # For renovate-sweep: it lists the ok majors of the repos this pass
        # didn't see to (one whose pass failed among them), and tells its
        # jobs how every repo's proofs run.
        save("repos.json", C.coverage(repos, covered))
        save("known.json", sorted(known))
        save("baseline-failed.json", failed)
    if not DRY:
        V.sweep(read, live)
    if kick is None:
        R.unhook_gone(repos + R.hook_projects(repos))
    # Last: GitHub failing it leaves the pass's state saved. A line more than
    # one repo has (a reviewer out of service) is listed once. The stalls the
    # watchdog took past a Claude job go on the stuck issue with the rest.
    R.publish(list(dict.fromkeys(tuple(x) for repo in sorted(digest) for x in digest[repo])) + waits.escalated())


if __name__ == "__main__":
    main()
