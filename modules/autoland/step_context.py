"""Shared state and status bookkeeping for one PR in one steward pass."""

import re
import time
import common as C
import stages as S
import stopping
import train as T
import waits
from common import CFG, DRY, MIN, load, log, repo_cfg, ts
from tasks import judged_on
from prs import MERGE_STEPS, post_status, show_waiting

# The link Claude Code puts in a PR its session opens.
SESSION = re.compile(r"https://claude\.ai/code/session_\w+")
# What a fix job of each kind is on, for a PR's status while another waits.
ON = {
    "handed": "its findings",
    "redFixed": "its red CI",
    "failFixed": "what its judge found failing",
    "conflictFixed": "its conflicts with its base",
}
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
    "build": "its proof build or run can't start (systemd-run refuses it, or a run has no checkout here)",
    "eval": "what its proof builds can't be worked out (its eval fails or times out)",
    "post": "GitHub keeps refusing a review it's to post",
    "answer": "GitHub keeps refusing a Claude job's answers it's to post",
    "lost": "its Claude jobs keep stopping before they finish (timed out, quiet or crashed)",
    "stop": "proj keeps refusing to stop its Claude jobs on a head it no longer has",
}
# What a status run takes from the live passes' state of a PR (Context).
LIVE_FACTS = ("strikes", "proofOutputs", "mergedOutputs")
STRIKE_LIMIT = 3  # failures in a row of one step, on one head, that make its wait overdue
CHECKS_START = 5 * 60  # how long GitHub gets to show a new head's first check
# What the steward's own proofs are doing while they're at work.
VERB = {"build": "building", "run": "running"}


def clock(t):
    """A time in a PR's status, in the machine's own time zone: 6:47pm PDT."""
    return time.strftime("%-I:%M%p %Z", time.localtime(t)).replace("AM", "am").replace("PM", "pm")


_HEAD = object()  # A job method defaults to this pass's head.


class Context:
    def __init__(self, repo, pr, repo_info, digest, held=None, below=None, *, land, onto):
        self.land = land
        self.onto = onto
        self.repo = repo
        self.pr = pr
        self.repo_info = repo_info
        self.digest = digest
        self.held = held
        self.below = below
        self.first = False  # step.handle's first
        self.rc = repo_cfg(self.repo)
        self.n, self.head = self.pr["number"], self.pr["headRefOid"]
        self.by_train = T.rides(self.repo, self.pr, self.repo_info)
        # A judge's verdict (and its job) is for this head on this base, with this
        # description (judged_on): one retargeted or redescribed is judged again.
        self.judged_at = f"{self.head} {judged_on(self.pr)}"
        self.key = f"pr-{self.repo.replace('/', '_')}-{self.n}.json"
        self.st = load(self.key, {})
        # Findings a reviewer couldn't post: by reviewer, then base (on another
        # base the PR is another change), the head they were on, until it posts
        # a look at the PR's head on that base. Kept across heads, since past
        # the round limit no new head's look would cover them. State from
        # before kept the loss in its head's rv: moved here before a new head
        # clears that.
        self.lost_on = self.st.setdefault("lostOn", {})
        for r, rs in self.st.get("rv", {}).items():
            if rs.get("lost"):
                was, base = rs.pop("lost")
                self.lost_on.setdefault(r, {})[base] = was
        if self.st.get("head") != self.head:
            stopping.moved(self.repo, self.pr, self.st)  # its jobs on the last head, to be stopped below
            self.st.update(head=self.head, seen=C.NOW, handed=None, strikes={}, rv={})
        if DRY:
            # A status run keeps state of its own, but takes what the live
            # passes found on this head from theirs: their failures in a row,
            # not the status runs' own, and what its proof builds, which a
            # status run never works out (proofs.outputs).
            live = load(self.key, {}, live=True)
            for k in LIVE_FACTS:
                if live.get("head") == self.head and k in live:
                    self.st[k] = live[k]
                else:
                    self.st.pop(k, None)
        self.st.setdefault("rv", {})
        self.labels = {x["name"] for x in self.pr["labels"]["nodes"]}
        self.merging = MERGE_STEPS.get(self.pr.get("mergeStateStatus"), "refused")
        self.age = C.NOW - self.st["seen"]
        self.tag = f"{self.repo}#{self.n}"
        self.line = f"[{self.repo}#{self.n}]({self.pr['url']}) {self.pr['title']}"

        self.refresh = {}  # refresh facts exported to the watchdog
        self.struck = {}  # the steps that failed this pass, and when each is tried again
        self.ends = {}  # how this head's job of each kind ended, as proj says (running)
        self.looked = {}  # what running said of each job this pass, by its id
        self.awaits = {}  # what each kind's fix job, its turn over, awaits elsewhere (running)

    def after(self, t, minutes):
        """A wait that ends `minutes` after t: this repo gets a pass then."""
        C.wake(self.repo, t + minutes * MIN + 1)  # ends: as each call says

    def strike(self, kind, said=None):
        """The step (STRIKES) failed, GitHub saying why when it said: tried
        again on a pass booked later each time, a minute on, then two,
        four... up to an hour, so a lasting failure costs little. Its
        STRIKE_LIMIT-th failure in a row on this head makes the PR's wait
        overdue whatever its deadline, so the watchdog takes it (a kick, a
        Claude job, the stuck issue); without the watchdog, it's listed on
        the stuck issue. Either says what GitHub said. Once a pass: two
        reviews it couldn't post are one pass's failure. When it's tried
        again."""
        if kind in self.struck:
            return self.struck[kind]
        strikes = self.st.setdefault("strikes", {})
        strikes[kind] = times = strikes.get(kind, 0) + 1
        log(f"{self.tag}: {STRIKES[kind]} ({times} in a row){f': {said}' if said else ''}")
        minutes = min(2 ** (times - 1), 60)
        self.struck[kind] = C.NOW + minutes * MIN
        self.after(C.NOW, minutes)  # ends: STRIKE_LIMIT in a row make the wait overdue
        if times >= STRIKE_LIMIT and not waits.overdue(self.repo, self.pr, STRIKES[kind], said):
            self.digest.append(
                (
                    "Failing again and again",
                    self.line,
                    f"{STRIKES[kind]}, {times} times in a row" + (f": {said}" if said else " (journalctl -u autoland)"),
                )
            )
        return self.struck[kind]

    def cleared(self, kind):
        """The step went through: its run of failures is over, unless it
        failed this pass too (one review posted, another not)."""
        if kind not in self.struck:
            self.st.setdefault("strikes", {}).pop(kind, None)

    def handoff_at(self):
        """When findings or red CI will have sat long enough for a Claude job
        to take them (a pass is booked for then). A PR that links the Claude
        session that opened it is that session's to settle: it hears of
        every finding and failure and is likely at work on them, so a job
        takes over only after a longer quiet, in case the session is gone.
        Every push or reply starts either wait over."""
        wait = self.rc["sessionHandoffAfter"] if self.session else self.rc["handoffAfter"]
        since = max(ts(self.pr["updatedAt"]), self.st["seen"])
        self.after(since, wait)  # ends: a Claude job takes it then
        return since + wait * MIN

    def settle(self, state, desc, why=None, *, stage, wait=waits.STEWARD):
        """The PR's status, what it waits on for the watchdog (waits.py), and
        the label of its stage (stages.py), which every status names. False
        if GitHub didn't take the status."""
        ok = post_status(self.repo, self.pr, state, desc)
        # A draft or a held PR is out of the pipeline, and GitHub's draft
        # mark or the hold label says so: no stage's label, whatever said it.
        show_waiting(self.repo, self.pr, self.labels, S.NONE if self.pr["isDraft"] or CFG["labels"]["hold"] in self.labels else stage)
        if why:
            self.digest.append((why, self.line, desc))
        self.refresh["usage_release"] = self.usage_grace()
        waits.note(self.repo, self.pr, desc, wait, jobs=self.looked, **self.refresh)
        # Not settled (any more): nothing may merge it.
        if state != "success" and (C.queued(self.pr) or self.st.get("landed")):
            if C.unland(self.repo, self.pr):
                self.st["landed"] = None
            else:
                ok = False
        if ok or DRY:
            self.cleared("status")
        else:
            # GitHub didn't take the status or the stop: until it does, the
            # old ones stand and a merge queued on it can still go.
            self.strike("status")
        # Nor may its train land it. Out after the status: cn reads it right
        # before each landing and takes out a head the steward no longer
        # says is settled (Conary#55). Still in, with the old status
        # standing, its train isn't stepped.
        if state != "success" and T.seat(self.repo, self.n) is not None and not T.leave(self.repo, self.pr) and not ok:
            T.stop(self.repo, f"#{self.n} isn't settled, and GitHub hasn't taken its status")
        return ok
