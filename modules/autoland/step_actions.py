"""Actions on a PR for a steward pass's shared context."""

import subprocess
import claims
import common as C
import proofs as P
import stages as S
import train as T
import waits
from common import CFG, DEP_RE, MIN, log
from seal import MAJOR as MAJOR_VERDICT
from touches import bot_edited
from tasks import FAILURES, MISDESCRIBED, REVIEWED, fix_task, judge_task, judgment, negative, proof_task, red_fix, red_undos
from prs import eyes, our_status
from step_context import ON, VERB, clock, _HEAD


class Actions:
    def update_branch(self, proof=False):
        """Bring a settled head up to date with a base whose rules want it
        (the merge won't), or whose builds its proof wants (proof: prove saw
        it behind): the merged-in head is reviewed like any other.
        GitHub makes it a moment later, and its push kicks a pass; one that
        hasn't come ten minutes on is asked for again. True while it's being
        brought up to date, False if it isn't behind, None if GitHub didn't
        take the ask (strike)."""
        if self.merging != "update" and not proof:
            return False
        if self.st.get("updated") == self.head and C.NOW - self.st.get("updatedAt", 0) < 10 * MIN:
            self.after(self.st["updatedAt"], 10)  # ends: a wait that stays the same is overdue at its deadline
            return True
        try:
            C.write("api", "-X", "PUT", f"repos/{self.repo}/pulls/{self.n}/update-branch", "-f", f"expected_head_sha={self.head}")
        except RuntimeError as e:
            self.strike("update", C.said(e))
            return None
        self.cleared("update")
        log(f"{self.tag}: behind its base, brought up to date")
        self.st["updated"], self.st["updatedAt"] = self.head, C.NOW
        self.after(C.NOW, 10)  # ends: a wait that stays the same is overdue at its deadline (waits.py)
        return True

    def hand_off(self, why, desc, kind="handed", at=_HEAD, wait=True, ours=True, describe=False):
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
        branch push over each other. describe: only its description is to
        fix, with no push (tasks.describe_task)."""
        if at is _HEAD:
            at = self.head
        ours = ours and not self.requested  # Renovate would redo its branch
        retry = self.st.get(kind) == at
        state = retry and self.running(kind, at)
        gone = retry and not state and self.lost(kind, at)
        how = gone and self.broke(kind)
        if gone and not how and self.st.get(kind + "Again") == at:
            self.settle(
                "failure", f"Two Claude jobs ended without fixing it: {desc}"[:140], "Two Claude jobs ended without fixing it", stage=S.FIX
            )
            return
        if state:
            who = f"a Claude job {'is queued for it' if state == 'queued' else 'has it'} since {clock(self.st[kind + 'At'])}"
        elif retry and not gone:
            who = "its Claude job just ended"  # the second look is booked
        elif how and (due := self.retry_at(kind)) > C.NOW:
            who = f"the last Claude job on it {how}, so another takes it at {clock(due)}"
        elif not ours:
            who = ""
        elif wait and (due := self.handoff_at()) >= C.NOW:
            who = (
                f"the Claude session that opened it has it until {clock(due)}, then a Claude job"
                if self.session
                else f"a Claude job takes it at {clock(due)}"
            )
        elif busy := self.fixing():
            log(f"{self.tag}: a Claude job ({busy}) is still working on this head; {kind} waits for it")
            who = f"a Claude job takes it once the one on {ON[busy]} ends"
        else:
            # A conflict's merge is a new head, whose proofs run again anyway:
            # its job doesn't rerun them, which can outlast it like the gate.
            conflict = kind == "conflictFixed"
            proofs = {c: self.machine[c] for c in self.ci_names if c in self.machine} if self.ci == "red" and not conflict else {}
            train = self.ci == "red" and any(c.startswith(f"{T.CONTEXT} (") for c in self.ci_names)
            before = claims.unposted(self.st, kind, at)
            red_negative = negative(self.ci_names) if self.ci == "red" and not conflict else None
            if self.started(
                kind, fix_task(self.repo, self.pr, why, proofs, train, self.tested, before, red_negative, describe), at, bool(how)
            ):
                if retry:
                    if not how:
                        self.st[kind + "Again"] = at
                    self.st.pop(kind + "Lost", None)
                if kind == "handed":
                    eyes(self.repo, self.pr)
                who = f"a Claude job has it since {clock(C.NOW)}" + ("; the last one's answers didn't go up" if before else "")
            else:
                self.digest.append(("Couldn't start a Claude job to fix it", self.line, why))
                who = "no Claude job could start"
        self.settle("pending", f"{desc}; {who}" if who else desc, stage=S.FIX)

    def merge(self):
        # Marked landed only once GitHub has done the merge, so a failed one
        # is tried again (strike), saying why.
        refused = self.land(self.repo, self.pr, self.repo_info)
        if refused is None:
            self.st["landed"] = self.head
            self.cleared("merge")
            return
        self.strike("merge", refused)

    def already_landed(self, tip):
        """Its merge would change nothing on its base, at tip (onto):
        taken back from anything that would merge it, and closed, saying so.
        A dependency bot's is left to its bot, which closes its own once it
        sees there's nothing left to update: one closed under it is an update
        it would ignore from then on."""
        said = f"{self.pr['baseRefName']} at {tip[:7]} already has everything it changes"
        if self.dep:
            self.settle("pending", f"Already landed: {said}; its bot closes it"[:140], stage=S.MERGE)
            return
        self.settle("pending", f"Already landed: {said}; closing it"[:140], stage=S.MERGE)
        if (
            C.write(
                "pr",
                "close",
                str(self.n),
                "-R",
                self.repo,
                "--comment",
                f"Closed without merging: {said}, so merging this would only add an empty commit. "
                "What it does landed another way, most likely through another PR.",
                check=False,
            )
            is None
        ):
            self.strike("close")
            return
        self.cleared("close")
        log(f"{self.tag}: {said}; closed it")

    def mergeable(self):
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
            tip, nothing = self.onto(self.repo, self.pr)
        except (RuntimeError, OSError, subprocess.SubprocessError) as e:
            self.settle("pending", "Settled, but what its merge would change can't be worked out; trying again", stage=S.MERGE)
            self.strike("empty", str(e))
            return False
        self.cleared("empty")
        if nothing:
            self.already_landed(tip)
            return False
        default = self.repo_info["defaultBranchRef"]["name"]
        if (
            tip
            and red_fix(self.pr, default, red_undos(self.repo))
            and tip != ((self.repo_info["defaultBranchRef"].get("target") or {}).get("oid"))
        ):
            self.settle(
                "pending", f"Waiting for CI on {default}: it lands only while {default} is red, and it's moved on"[:140], stage=S.BASE
            )
            return False
        return True

    def land_now(self, desc):
        """Settled: merged, unless what GitHub says of its merge asks for
        something first (MERGE_STEPS). Behind a base whose rules want it up
        to date, it's brought up to date instead, and the merged-in head is
        reviewed and its CI run like any other before it lands. (One that
        conflicts with its base never gets here: it's handed on first.) In a
        repo with its merge train on, one against the default branch goes in
        the train instead (train.py's ride). First of all, only one anything
        may merge now (mergeable)."""
        if not self.mergeable():
            return
        if self.merging == "update":
            why = "brought up to date" if self.update_branch() else "GitHub didn't take the update; trying again"
            self.settle("pending", f"Settled, but behind its base: {why}", stage=S.MERGE)
            return
        if self.by_train:
            # The train merges it onto the default branch itself, so what
            # GitHub says of merging it alone is no wait of its own: GitHub
            # works that out afresh each time the branch moves, which the
            # train's every landing does.
            T.ride(
                self.repo,
                self.pr,
                self.st,
                desc,
                self.settle,
                self.strike,
                our_status(self.pr, T.CONTEXT)[1],
                not (self.dep and not self.fixed),
            )
            return
        if self.merging in ("wait", "refused"):
            self.settle(
                "pending",
                "Settled; GitHub is still working out whether it can be merged"
                if self.merging == "wait"
                else f"Settled, but GitHub won't say it can be merged ({self.pr.get('mergeStateStatus')})",
                stage=S.MERGE,
            )
            self.strike("mergeable")
            return
        self.cleared("mergeable")
        if T.seat(self.repo, self.n) is not None:
            # Retargeted since it joined: the train lands only onto the
            # default branch, so it comes out and is merged here.
            T.leave(self.repo, self.pr)
        self.settle("success", desc, stage=S.MERGE)
        # Still open with a merge queued on it: GitHub's to merge later, which
        # land never leaves it to (a steward before it did). Merged now instead.
        if self.st.get("landed") == self.head and C.queued(self.pr):
            self.st["landed"] = None
        if self.st.get("landed") != self.head:
            log(f"{self.tag}: settled, landing")
            self.merge()

    def conflicted(self):
        """It conflicts with its base, so GitHub can't merge it and its CI
        doesn't run: a Claude job merges the base in, at once (one a head on
        its base, so one retargeted gets its own), and its push is reviewed
        like any other head. A dependency bot's own branch is the
        bot's to rebase, and only its own: one with anyone else's commit on
        it the bot leaves alone, major or minor. One renovate-sweep has (CI
        red, or a major with no verdict on this head) is its job's to bring
        up to date, and one it held is the owner's: no second job pushes to
        either branch."""
        base = self.pr["baseRefName"]
        verdict = C.marker(self.pr, MAJOR_VERDICT, self.head, self.repo, base) if self.dep == "major" and self.renovate else None
        if self.dep and not bot_edited(self.repo, self.pr, self.st, self.renovate):
            self.settle("pending", f"Conflicts with {base}; its bot rebases it", stage=S.FIX)
            self.open_a_day(f"Dependency update; conflicts with {base}", swept=self.renovate)
        elif verdict == "held":
            self.settle("pending", "Major update; renovate-sweep held it: see its comment", stage=S.OWNER, wait=None)
        elif self.renovate and self.dep and (self.ci == "red" or self.dep == "major" and verdict != "ok"):
            self.settle("pending", f"Conflicts with {base}; renovate-sweep has it", stage=S.FIX, wait=waits.SWEEP)
        else:
            self.hand_off(
                f"it conflicts with {base}, so GitHub can't merge it and its CI doesn't run: merge {base} into the "
                "branch (a merge commit: never rebase or force-push), resolve every conflict keeping what both sides "
                "meant. If the two sides can't both be kept, which one stays "
                f"is the owner's call. Just before you merge, and again before you push, check that the PR is still open "
                f"on {base} at head {self.head}: if its base or head moved on, stop without merging or pushing, since the "
                "steward hands what it is now to a new job",
                f"Conflicts with {base}",
                "conflictFixed",
                f"{self.head} {base}",
                wait=False,
            )
            # A job stuck in line, or one whose state can't be read, doesn't
            # keep it off the list.
            self.open_a_day(f"Conflicts with {base}")

    def judged(self, why, seen=REVIEWED):
        """Settled, but not something the steward lands on its own word: one
        Claude job a head judges it, and its verdict on this head decides."""
        verdict = judgment(self.pr, self.head, self.repo)
        if verdict == "land":
            self.land_now(f"Settled; a Claude job judged it fine to land ({why})")
        elif verdict == "owner" and self.st.get("ownerSaw") == self.judged_at:
            self.land_now(f"Settled; a Claude job sent it to {CFG['owner']}, who took the {CFG['labels']['owner']} label off ({why})")
        elif verdict == "owner":
            # Labelled here, never by the judge, so the steward knows it went
            # on: the owner has a label to take off, and taking it off is
            # their answer even before a pass has seen it on.
            self.settle(
                "pending", f"Settled; a Claude job judged it {CFG['owner']}'s to decide ({why})", "Judged yours to decide", stage=S.OWNER
            )
            if C.write("pr", "edit", str(self.n), "-R", self.repo, "--add-label", CFG["labels"]["owner"], check=False) is not None:
                self.st["ownerSaw"] = self.judged_at
        elif verdict in FAILURES:
            # The judge found this head failing: a fix job has it, and its push
            # (its edited description, if only that failed) is judged again. Its own
            # once a head: a job that answered a finding here unpushed didn't fix this.
            self.hand_off(*FAILURES[verdict], "failFixed", self.judged_at, wait=False, describe=verdict == MISDESCRIBED)
        elif self.update_branch():
            self.settle("pending", "Settled, but behind its base: brought up to date, reviewed again", stage=S.REVIEW)
        elif self.lost("judged", self.judged_at):
            self.again("judged", judge_task(self.repo, self.pr, why, self.ci == "none", seen), "judging it", "a verdict", self.judged_at)
        elif self.st.get("judged") == self.judged_at or self.started(
            "judged", judge_task(self.repo, self.pr, why, self.ci == "none", seen), self.judged_at
        ):
            self.settle("pending", f"Settled; {self.at_work('judged', 'judging it', self.judged_at)} ({why})", stage=S.REVIEW)
        else:
            self.settle(
                "pending", f"Settled, but no Claude job could judge it ({why})", "Couldn't start a Claude job to judge it", stage=S.REVIEW
            )

    def prove(self):
        """The reviewers are done, and checks that run on a machine, not in
        CI, are left. Those with a proofBuilds attribute go first, built by
        the steward itself (proofs.build), no Claude job, while CI still
        runs (build_early); then those with a proofRuns command, run by the
        steward itself the same way (proofs.start_run); the rest, once CI,
        the builds and the runs have passed, a Claude job runs on this head
        and records their verdicts. Each starts once its repo's proofSlots
        have room for it (one started again too), or a build has nothing
        new to build; a build in line builds what it's built from first,
        outside the line (proofs.prebuild); with nothing to build at all,
        none: the verdicts are taken from the store (proofs.py)."""
        builds = [c for c in self.ci_names if c in self.rc["proofBuilds"]]
        runs = [] if builds else [c for c in self.ci_names if c in self.rc["proofRuns"]]
        beside = [c for c in self.ci_names if c not in self.machine]  # CI still running
        ci_too = f", CI ({', '.join(beside)}) beside it" if beside else ""
        checks = builds or runs or self.ci_names
        names = ", ".join(checks)
        # The steward's own proof, a build or a run, else a Claude job's.
        unit = "build" if builds else "run" if runs else None
        if unit:
            doing, task = names, None

            def begin():
                return (P.build if builds else P.start_run)(self.repo, self.pr, checks, self.st)
        else:
            doing, task, begin = f"running {names}", proof_task(self.repo, self.pr, checks, self.rc["proofJobs"]), None

        def running_now():
            skipped = "; nothing new to build, so it skipped the line" if self.st.get("provedFree") == self.head else ""
            busy = f"{VERB[unit]} {names} here" if unit else self.at_work("proved", doing)
            self.settle("pending", f"Settled; {busy}{ci_too}{skipped}"[:140], stage=S.PROOF)

        # What this proof is: a build, a run or a Claude job, of which
        # checks, each built from which attribute or run as which command.
        # This head's proof so far was another (proofs.fits: a build before
        # the Claude jobs, a job from before its check had a build, a check
        # added or remapped since): this one starts afresh, nothing of that
        # one's carried over (retries, lost answers, verdicts, a prebuild:
        # proofs.drop_inputs). One at work is stopped (the stopping path when
        # proj or systemd won't), and holds its slot till over (proofs.live).
        stage = [unit or "job", sorted(checks), {c: self.rc["proofBuilds" if builds else "proofRuns"].get(c) for c in checks}]
        P.drop_inputs(self.st, [self.head, stage])
        if self.st.get("proved") == self.head and (jid := self.st.get("provedJob")) and not P.fits(self.st, stage):
            self.st["proofsLive"] = P.jobs(self.st)
            if self.running("proved") and not C.stop_job(jid):
                self.st["stopping"] = list(dict.fromkeys(self.st.get("stopping", []) + [jid]))
            for k in ("", "Job", "At", "Lost", "Again", "Broke", "For", "Unposted"):
                self.st.pop("proved" + k, None)
            self.ends.pop("proved", None)
        # A build or run of this head that's done: the verdicts it kept. What
        # goes on the PR (a failed build's log, a run's summary) goes first,
        # so a comment GitHub didn't take is tried again before the red
        # status hands the PR to a fix; verdicts GitHub took only some of
        # are posted again even once it's red (handle).
        if unit and self.st.get("proved") == self.head and not self.running("proved") and self.ends.get("proved") == "done":
            jid = self.st["provedJob"]
            said = P.verdicts(jid)
            if said is not None:
                # Only what's left to post: the ones it posted are green.
                said = {c: v for c, v in said.items() if c in checks}
            if said is None:
                # One that kept no verdicts broke: it's done again (below).
                self.ends["proved"] = "failed"
            else:
                note = P.note(jid, said) if self.st.get("provedSaid") != jid else None
                if note is not None and C.comment(self.repo, self.n, note) is not None:
                    self.st["provedSaid"] = jid
                said_all = note is None or self.st.get("provedSaid") == jid
                done = "built" if builds else "ran"
                if said_all and P.record(self.repo, self.pr, said):
                    self.st.pop("provedUnposted", None)
                    # Their statuses kick the pass that lands it, or hands it to a fix.
                    self.settle("pending", f"Settled; {names} {done} here, its verdict posted"[:140], stage=S.PROOF)
                    return
                if said_all:
                    self.st["provedUnposted"] = jid
                self.strike("status")
                self.settle("pending", f"Settled; {names} {done} here, its verdict not posted yet"[:140], stage=S.PROOF)
                return
        retry = self.lost("proved")
        if not retry and self.st.get("proved") == self.head:
            running_now()
            return
        if (drvs := P.built(self.repo, self.pr, checks, self.st)) and not P.record(self.repo, self.pr, P.stored(checks, drvs)):
            self.strike("status")
        if drvs:  # Their statuses kick the pass that lands it.
            self.settle("pending", f"Settled; {names} taken from the store, not run again"[:140], stage=S.PROOF)
            return
        # One that broke is counted, and its retry booked, whether or not
        # there's room: only the job started in its place waits for a slot.
        if retry and self.broke("proved"):
            self.retry_at("proved")
        slots = self.rc.get("proofSlots")
        outs = P.outputs(self.repo, self.pr, checks, self.st) if slots or builds else None
        if builds and outs is None:
            # Nothing to build without its derivations: tried again on a
            # later pass (outputs).
            self.strike("eval")
            self.settle("pending", f"Settled, but what {names} builds can't be worked out here yet"[:140], stage=S.PROOF)
            return
        if builds:
            self.cleared("eval")
        # Behind its base (the default branch, whose tip this pass read),
        # with what it builds merged in all built already: brought up to
        # date, its new head's verdicts come from the store. Not a
        # dependency update's: its bot keeps that branch.
        default = self.repo_info.get("defaultBranchRef") or {}
        tip = (default.get("target") or {}).get("oid") if self.pr["baseRefName"] == default.get("name") else None
        if (
            builds
            and not retry
            and not DEP_RE.match(self.pr["headRefName"])
            and (up := P.merged(self.repo, self.pr, checks, self.st, tip))
            and not P.unbuilt(up)
        ):
            if self.update_branch(proof=True):
                self.settle(
                    "pending",
                    f"Settled; brought up to date with {self.pr['baseRefName']}, which has {names} built here already"[:140],
                    stage=S.PROOF,
                )
                return
            self.settle("pending", "Settled; GitHub didn't take its update with its base, so trying again"[:140], stage=S.PROOF)
            return
        free = False
        # One whose next step is failing the PR (again) needs no slot.
        last = retry and self.st.get("provedAgain") == self.head and not self.broke("proved")
        ahead = P.ahead(self.repo, self.n, self.st, slots, outs) if slots and not last else None
        # A build in line builds what it's built from first, outside the line
        # (proofs.prebuild); one whose outputs a proof is building needn't.
        if builds and slots and not last and ahead != P.FREE and (pre := P.prebuild(self.repo, self.pr, checks, self.st, stage)):
            if pre == "building":
                self.cleared("build")
                self.settle("pending", f"Settled; building what {names} is built from, before its turn{ci_too}"[:140], stage=S.PROOF_SLOT)
            else:
                self.strike("build")
                self.settle(
                    "pending",
                    f"Settled, but the build of what {names} is built from couldn't start"[:140],
                    f"Couldn't start a proof build of what {names} is built from",
                    stage=S.PROOF,
                )
            return
        if ahead is not None:
            if ahead != P.FREE:
                P.wait(self.repo, self.n)
                self.settle(
                    "pending",
                    f"Settled; waiting its turn to {'build' if builds else 'run'} {names} ({ahead} ahead)"[:140],
                    stage=S.PROOF_SLOT,
                )
                return
            free = True
        before = self.st.get("provedJob")
        self.st["provedFree"] = self.head if free else None
        if retry:
            self.again("proved", task, doing, "its verdict", begin=begin, unit=unit)
        elif self.started("proved", task, begin=begin):
            running_now()
        elif unit:
            self.settle(
                "pending", f"Settled, but the {unit} of {names} couldn't start"[:140], f"Couldn't start a proof {unit}", stage=S.PROOF
            )
        else:
            self.settle(
                "pending",
                f"Settled, but no Claude job could run {names}"[:140],
                "Couldn't start a Claude job to run its checks",
                stage=S.PROOF,
            )
        if self.st.get("provedJob") != before:
            self.st["provedFor"] = stage
        P.started(self.st, outs if self.st.get("provedJob") != before else None)

    def gate(self):  # what makes it R2, from Conary's class policy (train.py's gate)
        return T.gate(self.repo, self.pr)
