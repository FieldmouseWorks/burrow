"""Job lifecycle bookkeeping for a steward pass's shared context."""

import claims
import common as C
import proofs as P
import stages as S
import waits
from common import CFG, MIN, log, ts
from step_context import STRIKES, STRIKE_LIMIT, ON, VERB, clock, _HEAD

# What a fix job between turns on its own PR awaits, as its status says it.
OWN = "this PR's next change"


class Jobs:
    def usage_grace(self):
        """Only jobs still owning a step at settlement contribute grace."""
        current = {jid for key, jid in self.st.items() if key.endswith("Job") and self.st.get(key[:-3])}
        records = self.st.get("usageJobs", {})
        return max((record["release"] for jid, record in records.items()
                    if jid in current and self.looked.get(jid) in ("usage", "queued", "running", "unknown")), default=None)

    def deadline(self, kind):
        """A pass when this kind's job is due to have ended (proj stops a turn
        at jobTimeout, a proof build stops at proofTimeout), and every
        quarter hour past that while it still hasn't (it waited for a slot
        first): its end kicks one, and this is the one that sees it if that
        kick never got through."""
        limit = P.timeout(self.repo) if str(self.st.get(kind + "Job")).startswith(C.BUILD) else CFG["jobTimeout"]
        due = self.st.get(kind + "At", C.NOW) + (limit + 5) * MIN
        C.wake(self.repo, due if due > C.NOW else C.NOW + 15 * MIN)  # ends: its PR's wait, unchanged, is overdue at its deadline

    def running(self, kind, at=_HEAD):
        """This head's job of the kind (at: on its base, for the judge's and
        the fix it asks for) while it's still queued or running, or proj
        can't say: "queued", "running" or "unknown"; else "", and how it
        ended goes in ends. One that is keeps its deadline, since each pass
        replaces the repo's wake-ups. A job of any kind whose turn ended
        holding something open (C.job_lifecycle: a question for a person,
        or a PR or issue other than this one it awaits, the upstream fix it
        needs, say) is "awaiting", what it waits on in awaits: still at work
        here, since proj resumes it once that's answered or changes, so it's
        neither lost nor started again (remi#881: two jobs that ended
        without fixing it, while both waited on burrow's PR with the fix,
        and a question's job taken for ended, so a second fixed the same
        branch while the first's question timed out and resumed it). One
        look a job a pass: asked again, it says the same, and a run of
        broken jobs is counted once (breaking)."""
        if at is _HEAD:
            at = self.head
        jid = self.st.get(kind + "Job")
        if self.st.get(kind) != at or not jid:
            return ""
        if jid not in self.looked:
            self.looked[jid] = self.look(kind, jid)
        return self.looked[jid]

    def look(self, kind, jid):
        """running's look at the job: what proj says of it, once."""
        snapshot = C.lifecycle(jid)
        state, target = C.job_lifecycle(jid, self.tag, snapshot)
        if (not target and kind in claims.KINDS and state not in (None, "queued", "running")
                and (self.st.get("answering") or {}).get(jid, {}).get("owns")):
            # A fix job claims.rs says may still push to the branch (resumed
            # by what its answers put up, or retiring until proj confirms
            # it's over): none other is handed the branch meanwhile.
            target = OWN
        usage = C.job_usage(jid, snapshot)
        records = self.st.setdefault("usageJobs", {})
        self.st.pop("usageRelease", None)  # replaced by job-scoped releases
        release = C.job_release(snapshot)
        if state is None and jid in records:
            record = records[jid]
            if record["release"] > C.NOW:
                usage = record["engine"], record["release"]
            C.wake(self.repo, min(C.NOW + MIN, record["release"]) if record["release"] > C.NOW else C.NOW + MIN)  # ends: retry the unreadable lifecycle, bounded by reset
        if usage:
            engine, reset = usage
            records[jid] = {"engine": engine, "release": reset}
        elif jid in records and state is not None:
            if state in ("queued", "running") and not target:
                records[jid]["release"] = min(records[jid]["release"], release if release is not None else C.NOW)
            else:
                records.pop(jid)
        if usage:
            engine, reset = usage
            if reset > C.NOW or (state not in ("queued", "running") and target == "usage-limit"):
                if reset > C.NOW:
                    C.wake(self.repo, reset)  # ends: the engine resets, and proj resumes this job
                else:
                    self.deadline(kind)
                self.awaits[kind] = f"{engine}'s usage reset at {clock(reset)}"
                return "usage"
        if target:
            # No deadline: its wake is proj's resume, whose end kicks a pass.
            self.awaits[kind] = target
            return "awaiting"
        live = state in (None, "queued", "running")
        if not live:
            self.ends[kind] = state
        if live or self.broke(kind):
            self.breaking(kind)
        if not live:
            return ""
        self.deadline(kind)
        return state or "unknown"

    def at_work(self, kind, doing, at=_HEAD):
        """What the status says of this head's job of the kind: queued for
        its work until proj starts it (a job can wait hours for a slot, and
        proj kicks a pass when it starts), at it from then on, and what it
        waits on while its turn is over but it's still at work (running's
        "awaiting")."""
        if at is _HEAD:
            at = self.head
        state = self.running(kind, at)
        if state == "usage":
            return f"waiting for {self.awaits[kind]} to continue {doing}"
        if state == "queued":
            return f"a Claude job is queued for {doing}"
        if state == "awaiting":
            return f"the Claude job {doing} awaits {self.awaits[kind]}"
        return f"a Claude job is {doing}"

    def started(self, kind, task, at=_HEAD, broken=False, begin=None):
        """A Claude job, once per head (at: once per head on its base, for
        the judge's; recorded once it has started; until then it's tried
        again (strike), and its caller lists it on the stuck issue), or
        what begin starts in its place and gives the id of (a proof build).
        One that isn't in place of one that broke (retry_at) ends a run of
        those."""
        if at is _HEAD:
            at = self.head
        if kind in claims.KINDS and not claims.stand_down(self.repo, self.pr, self.st, self.tag, self.strike, self.cleared, self.after):
            log(f"{self.tag}: no new {kind} job while an earlier one on the branch won't stop")
            self.after(C.NOW, 1)  # ends: the next try to stop it
            return False
        jid = begin() if begin else C.job(self.repo, task, since=ts(self.pr["createdAt"]), role=f"steward-{kind}")
        step = "build" if begin else "job"
        if jid:
            self.st[kind] = at
            self.st[kind + "Job"] = jid
            self.st[kind + "At"] = C.NOW
            if kind in claims.KINDS:
                claims.handed(self.st, kind, jid, self.pr)
            self.deadline(kind)
            self.cleared(step)
            if not broken:
                self.cleared("lost")
            log(f"{self.tag}: {'a build' if begin else 'a Claude job'} {jid} ({kind})")
        else:
            log(f"{self.tag}: couldn't start {'its build' if begin else 'a Claude job (no checkout, or proj refused)'}")
            self.strike(step)
        return bool(jid)

    def lost(self, kind, at=_HEAD):
        """This head's job of the kind ended without the answer it was for
        (it crashed, timed out, or never posted it). Seen on two passes: the
        first may have read the PR just before its answer went up."""
        if at is _HEAD:
            at = self.head
        if self.st.get(kind) != at or not self.st.get(kind + "Job") or self.running(kind, at):
            return False
        seen = self.st.get(kind + "Lost") == at
        self.st[kind + "Lost"] = at
        if not seen:
            self.after(C.NOW, 1)  # ends: the second look, once
        return seen

    def broke(self, kind):
        """How this head's job of the kind ended, if it stopped before it
        finished its turn (it timed out, went quiet, crashed, or Claude
        errored), else None: that's the machine failing, not an answer."""
        how = self.ends.get(kind)
        if how in (None, "done"):
            return None
        return {"error": "ended in an error", "failed": "crashed"}.get(how, how)

    def retry_at(self, kind):
        """When this head's job of the kind that broke may start again: each
        such job is a strike, never counted as an answer, so the third on a
        head makes its wait overdue (the watchdog takes it), and the next
        starts on the pass the strike booked, a minute on, then two, four...
        up to an hour. Each pass replaces the repo's wake-ups, so every one
        that waits for it books that pass again."""
        if self.st.get(kind + "Broke", [None])[0] != self.st.get(kind + "At"):
            self.st[kind + "Broke"] = [self.st.get(kind + "At"), self.strike("lost")]
        self.breaking(kind)
        due = self.st[kind + "Broke"][1]
        if due > C.NOW:
            self.after(due, 0)  # ends: the retry, on its pass
        return due

    def breaking(self, kind):
        """Jobs on this head keep breaking: STRIKE_LIMIT or more in a row,
        this kind's among them. While its step is still this kind's job (one
        in place of one that broke, waiting to start or running, or another
        that broke), its wait stays overdue, as a step failing on every pass
        does (strike): the watchdog takes it, however long the retries go
        on. One that finishes its turn ends the run (started)."""
        times = self.st.get("strikes", {}).get("lost", 0)
        if times < STRIKE_LIMIT or not self.st.get(kind + "Broke") or "lost" in self.struck:
            return
        if not waits.overdue(self.repo, self.pr, STRIKES["lost"]):
            self.digest.append(
                ("Failing again and again", self.line, f"{STRIKES['lost']}, {times} times in a row (journalctl -u autoland)")
            )

    def again(self, kind, task, doing, answer, at=_HEAD, begin=None, unit=None):
        """A job that ended without its answer is started once more; one that
        does that twice fails the PR's status and is listed for the owner.
        One that broke is started again once its retry is due, however many
        times, and doesn't count toward the two (retry_at). begin: a proof
        build's or run's (unit), as started has it (doing is then what it
        builds or runs)."""
        if at is _HEAD:
            at = self.head
        how = self.broke(kind)
        last = f"{unit} of {doing}" if begin else f"Claude job {doing}"
        stage = S.PROOF if kind == "proved" else S.REVIEW  # a proof's, or a judge's
        if how and (due := self.retry_at(kind)) > C.NOW:
            self.settle("pending", f"Settled; the last {last} {how}, so another starts at {clock(due)}"[:140], stage=stage)
        elif not how and self.st.get(kind + "Again") == at:
            self.settle(
                "failure",
                f"Settled, but two Claude jobs ended without {answer}"[:140],
                f"Two Claude jobs ended without {answer}",
                stage=stage,
            )
        elif self.started(kind, task, at, bool(how), begin):
            if not how:
                self.st[kind + "Again"] = at
            self.st.pop(kind + "Lost", None)
            busy = f"{VERB[unit]} {doing} here" if begin else self.at_work(kind, doing, at)
            self.settle("pending", f"Settled; {busy} again (the last one {how or f'ended without {answer}'})"[:140], stage=stage)
        elif begin:
            self.settle(
                "pending", f"Settled, but the {unit} of {doing} couldn't start again"[:140], f"Couldn't start a proof {unit}", stage=stage
            )
        else:
            self.settle(
                "pending", f"Settled, but no Claude job could start {doing} again"[:140], "Couldn't start a Claude job", stage=stage
            )

    def fixing(self):
        """The kind of fix job still at work (running: queued, running,
        awaiting, or that proj can't say about), else None: whatever head or base it was handed on, since
        each pushes to the branch, and a new head or base doesn't stop it."""
        for k in ON:
            if self.st.get(k + "Job") and self.running(k, self.st.get(k)):
                return k
        return None
