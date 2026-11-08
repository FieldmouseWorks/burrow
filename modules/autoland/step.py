"""One PR's next step, for autoland-steward's pass (steward.py): where it
stands (its reviewers' verdicts, CI, its threads, its labels, what GitHub
says of its merge), and the thing to do now: queue a review, post what came
back, hand what's open (findings, red CI, a conflict with its base) to a
Claude job, have a job run its machine checks or judge it, post its
`review-settled` status and merge it (or, in a repo with its merge train
on, put it in the train, which lands it: train.py), or list it for the
owner or the stuck issue. Each step it takes is in the PR's status, in
what the watchdog watches (waits.py), and in the label of the stage it
leaves the PR at (stages.py), which every status names.

A step that fails (GitHub refuses it, or no job can start) is tried again,
later each time, and never quietly for ever: the third failure in a row on a
head makes the PR's wait overdue, so the watchdog takes it (strike). A Claude
job that stops before it finishes (it times out, goes quiet or crashes) is
the machine failing, not the job's answer: it's started again the same way
(retry_at)."""

import dependency_step
import claims
import common as C
import lock_wait
import proofs as P
import review_step
import reviewers as V
import stages as S
import stopping
import train as T
import waits
from aside import aside
from common import CFG, MIN, save, ts
from seal import MAJOR as MAJOR_VERDICT
from touches import automerge_spent, automerges, bot_edited, codex_written, dep_kind, gate_edit, moved_from, provenance
from tasks import DEP_SEEN, FAILURES, PAST_LIMIT, REVIEWED, judgment, owed
from tasks import fix_task as fix_task, judge_task as judge_task, proof_task as proof_task, negative as negative
from prs import ci_state, has_ci, our_status
from prs import onto, land, MERGE_STEPS as MERGE_STEPS
from step_context import Context, CHECKS_START, SESSION, clock, STRIKES as STRIKES
from step_jobs import Jobs
from step_actions import Actions


# Keep the entry point's existing helpers available to callers and test stand-ins.
__all__ = ["handle", "fix_task", "judge_task", "proof_task", "negative", "MERGE_STEPS", "STRIKES"]


def handle(repo, pr, repo_info, digest, held=None, below=None):
    Pass(repo, pr, repo_info, digest, held, below, land=land, onto=onto).run()


class Pass(Context, Jobs, Actions):
    def open_a_day(self, what, swept=False):
        """Anything not merged a day after it was opened, reviewed clean or
        not (a check that never reports keeps it pending), is listed, unless
        it's listed already, watchdog or not. A Renovate update's own stuck
        ones are renovate-sweep's (swept)."""
        if not swept and C.NOW - ts(self.pr["createdAt"]) > 86400 and not any(x[1] == self.line for x in self.digest):
            self.digest.append(("Open over a day", self.line, what))

    def owner_decides(self):
        """Sent to the owner by hand, by a fix job or by a judge (the label):
        listed for them, and no judge job."""
        judged_owner = judgment(self.pr, self.head, self.repo) == "owner"
        if judged_owner:
            # The judge's word, seen labelled on this head: once the label
            # is off, that's the owner's go-ahead. A label taken off before
            # the judge said anything isn't an answer to it.
            self.st["ownerSaw"] = self.judged_at
        self.settle(
            "pending",
            f"Settled; {CFG['owner']} decides (the {CFG['labels']['owner']} label)",
            "Judged yours to decide" if judged_owner else f"Labelled {CFG['labels']['owner']}",
            stage=S.OWNER,
        )

    def run(self):

        # The jobs a new head left answering for nothing, stopped: one proj
        # refuses is tried again.
        if stopping.stop(self.tag, self.st):
            self.cleared("stop")
        else:
            self.strike("stop")
        # Open a day, it's listed for the owner (below): a pass then says so.
        self.after(ts(self.pr["createdAt"]), 24 * 60)  # ends: listed for the owner

        self.session = bool(SESSION.search(self.pr.get("body") or ""))

        self.refresh = {}  # what the watchdog's to count of a refresh of it renovate-sweep has (waits.note)

        # What its fix jobs left, posted once it's so (or they're stopped).
        if said := claims.look(self.repo, self.pr, self.st, self.tag, self.strike, self.cleared, self.after):
            self.settle("pending", said, stage=S.FIX)
            save(self.key, self.st)
            return
        if set_aside := aside(self.tag, self.pr, self.labels, self.held, self.below):
            self.settle("pending", *set_aside[:2], stage=set_aside[2], wait=set_aside[3])
            save(self.key, self.st)
            return

        self.machine = C.machine(self.rc)  # the checks a machine runs, not CI, and how
        self.ci, self.ci_names, docs = ci_state(self.pr, self.machine, moved_from(self.repo, self.pr, self.st), self.repo)
        # A build's or run's verdicts GitHub took only some of: the rest go up
        # whatever CI says now, so a failure it took doesn't leave the others
        # unposted behind a red PR. Their statuses kick the pass that sees them.
        # Only those of its checks still that kind of proof, from the attribute
        # or command it had (proofs.still): one dropped, remapped or made the
        # other kind since waits for its own.
        if (left := self.st.get("provedUnposted")) and self.st.get("proved") == self.head:
            said = P.still(self.st, P.verdicts(left) or {}, self.rc)
            if not said or P.record(self.repo, self.pr, said):
                self.st.pop("provedUnposted")
            else:
                self.strike("status")
        # A head just pushed has no checks until GitHub has started them, which
        # isn't a repo with no CI: it gets a while to show them first, wherever
        # CI is wanted at all (below).
        if self.ci == "none" and self.rc["requireCi"] and not self.by_train:
            if (self.st.get("noChecks") or [None])[0] != self.head:
                self.st["noChecks"] = [self.head, C.NOW]
            if C.NOW < (due := self.st["noChecks"][1] + CHECKS_START):
                self.ci, self.ci_names = "pending", ["its first check"]
                C.wake(self.repo, due)  # ends: once per head, then it's a repo with no CI
        # No checks ran at all ("none"). Unless the repo is set not to need any
        # (docs only), that's not green: nothing has tested it, so a judge job
        # runs the build and tests itself. One the train lands, the train's CI
        # is what tests it, merged onto the default branch.
        self.tested = self.by_train or has_ci(self.pr, self.machine)  # what a fix job pushes: CI tests it, or only the job does
        if self.ci == "none" and (not self.rc["requireCi"] or self.by_train):
            self.ci = "green"
        open_threads = sum(1 for t in self.pr["reviewThreads"]["nodes"] if not t["isResolved"])
        latest = {}
        for r in self.pr["reviews"]["nodes"]:
            who = (r["author"] or {}).get("login")
            if who and r["state"] in ("APPROVED", "CHANGES_REQUESTED", "DISMISSED"):
                latest[who] = r["state"]
        changes = [w for w, s in latest.items() if s == "CHANGES_REQUESTED"]
        self.dep = dep_kind(self.pr)

        self.renovate = self.pr["headRefName"].startswith("renovate/")
        proof_only = self.ci == "pending" and bool(self.ci_names) and all(c in self.machine for c in self.ci_names)
        # A check the steward builds itself costs no Claude job, so it needn't
        # wait for CI to pass: it starts once the reviewers are done, and builds
        # while CI runs rather than after it. A Claude job's check still waits.
        build_early = self.ci == "pending" and any(c in self.rc["proofBuilds"] or c in self.rc["proofRuns"] for c in self.ci_names)
        # A minor dependency PR a Claude job fixed carries Claude's code, which no
        # reviewer has read (and Renovate won't merge); a bot's PR that changes a
        # workflow beyond repinning its actions changes the gate. Either goes
        # through the review loop below like a Claude PR. While a Renovate one's CI
        # is red its branch is renovate-sweep's: a job from each on one branch
        # would push over each other. The sweep's job answers no findings: once its
        # push is green they're handed over here like any PR's, to a job whose
        # answers go up only on its own push (claims).
        # A judge asked for on this head and base (the repo had no CI then, say)
        # still decides once CI shows up, or the description is edited (then on
        # the new one): its owner or failing verdict isn't dropped for either.
        judge_said = judgment(self.pr, self.head, self.repo)
        asked = owed(self.pr, self.head, self.repo, self.st.get("judged"))
        # Its owner or failing verdict holds it whatever CI is still doing; only
        # a land waits for CI.
        self.held = asked and judge_said in ("owner", *FAILURES)
        # Renovate merges its own once they're green, but none in a repo with its
        # train on (updates.nix), so it reads their commits for itself first.
        rechecked = self.renovate and not T.on(self.repo)
        self.fixed = (
            self.dep == "minor"
            and (bot_edited(self.repo, self.pr, self.st, rechecked) or gate_edit(self.repo, self.pr, self.st))
            and not self.gate()
        )
        swept = self.fixed and self.ci == "red" and self.renovate
        # The date a lock file refresh waits for, renovate-sweep's to have it refreshed then.
        self.requested = self.renovate and lock_wait.requested(self.pr, C.NOW, CFG["renovateRun"] or 0)
        self.refresh = lock_wait.refresh(self.pr, self.ci == "red", self.ci_names, C.renovated, self.requested) if self.renovate else {}

        # Dependency bumps: CI is the review. Renovate merges its own minor
        # ones; renovate-sweep fixes the red ones and gives each major a verdict,
        # and answers none of their threads. Anything else that's open on one, a
        # review thread or a red Dependabot update, goes to a Claude job like any
        # PR's.
        # Sent to the owner (by hand, a fix job or a judge): theirs, whatever else
        # is open on it, and nothing more is asked or handed off until the label
        # comes off.
        if CFG["labels"]["owner"] in self.labels:
            self.owner_decides()
            save(self.key, self.st)
            return
        # Conflicting with its base, nothing else on it can settle: GitHub won't
        # merge it and its CI doesn't run.
        if self.merging == "conflict":
            self.conflicted()
            save(self.key, self.st)
            return

        self.reviewed_round = False
        if self.dep and not self.fixed:
            dependency_step.step(
                self.repo, self.pr, self.st, dep=self.dep, renovate=self.renovate,
                head=self.head, labels=self.labels, refresh=self.refresh,
                open_threads=open_threads, changes=changes, ci=self.ci, ci_names=self.ci_names,
                held=self.held, proof_only=proof_only, build_early=build_early, asked=asked,
                gate=self.gate, reviewed=lambda: self.reviewed(open_threads, changes, swept),
                settle=self.settle, hand_off=self.hand_off, judged=self.judged,
                prove=self.prove, land_now=self.land_now, mergeable=self.mergeable, clock=clock)
            self.open_a_day(f"Dependency update, CI {self.ci}", swept=self.renovate)
            save(self.key, self.st)
            return

        looked = self.reviewed(open_threads, changes, swept)
        if looked is None:
            self.open_a_day(our_status(self.pr)[1] or "unsettled")
            save(self.key, self.st)
            return
        live, said, rounds, clean, capped, unseen, late, waiting, stuck, out_of_service, unreviewed = looked

        # 2. The reviewers are clean on this head and nothing is open: CI, then land.
        if self.ci == "red":
            # Its own once a head, apart from a findings job's: one that answered
            # a finding here without pushing didn't fix a check that failed since.
            self.hand_off(
                "CI is red (" + ", ".join(self.ci_names) + ")", "CI is red: " + ", ".join(self.ci_names)[:100], "redFixed", ours=not swept
            )
        elif self.held:
            self.judged("a Claude job was asked to judge this head", PAST_LIMIT if capped else REVIEWED)
        elif proof_only or build_early:
            self.prove()
        elif self.ci == "pending":
            self.settle("pending", "Settled; waiting for CI: " + ", ".join(self.ci_names)[:90], stage=S.CI)
        elif (codex := codex_written(self.repo, self.pr, self.st)) is None:
            self.settle("pending", "Settled; couldn't read its commits; next pass", stage=S.REVIEW)
        elif codex or self.gate() or unseen or self.ci == "none" or asked or capped:
            self.judged(
                "a Codex job wrote it"
                if codex is True
                else codex
                if codex
                else f"it touches {self.gate()}"
                if self.gate()
                else f"it changes {', '.join(unseen[:5])}{' and more' * (len(unseen) > 5)}, which the reviewers aren't shown"
                if unseen
                else "the repo has no CI"
                if self.ci == "none"
                else "a Claude job was asked to judge this head"
                if asked
                else f"it went past the review-round limit ({rounds} rounds with findings)",
                PAST_LIMIT if capped else REVIEWED,
            )
        else:
            self.land_now(
                (
                    "Documentation only, so no review round"
                    if unreviewed
                    else f"{' and '.join(V.NAMES[r] for r in live)} found nothing in this commit's change"
                )
                + f", every thread is resolved, CI is green{' (docs only)' * docs}"
            )

        self.open_a_day(our_status(self.pr)[1] or "unsettled")
        save(self.key, self.st)

    def reviewed(self, open_threads, changes, swept):
        # The reviewers (review_step.py).
        looked = review_step.look(
            self.repo,
            self.pr,
            self.key,
            self.st,
            self.lost_on,
            self.digest,
            self.line,
            self.age,
            self.after,
            self.strike,
            self.cleared,
            lambda: T.review(self.repo, self.pr),
        )

        live, said, rounds, clean, capped, unseen, late, waiting, stuck, out_of_service, unreviewed = looked

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
                self.hand_off(", ".join(what), "; ".join(waiting + stuck + what), ours=not swept)
            else:
                self.settle("pending", "; ".join(waiting + stuck), stage=S.REVIEWER if out_of_service else S.REVIEW)
        elif what and capped:
            self.hand_off(
                f"{', '.join(what)}, after {rounds} review rounds with findings, the limit, so its reviewers won't look again. "
                "Weigh each finding on its merits: fix what's a real defect and answer the rest with why. "
                "A Claude job judges it after, without the reviewers",
                f"Past the review-round limit ({rounds}): " + ", ".join(what),
                ours=not swept,
            )
        elif (not clean and not capped) or what:
            if not what:
                # Every finding answered without a new commit: the reviewers that
                # had findings look again.
                for r in live:
                    if said[r][0] == "findings":
                        self.after(said[r][1], self.rc["settleDelay"])  # ends: the look it asks for, once
                        if C.NOW - said[r][1] > self.rc["settleDelay"] * MIN:
                            V.ask(self.repo, self.pr, self.key, r, self.st["rv"].setdefault(r, {}), self.after)
                self.settle(
                    "pending", "Findings answered; " + ("; ".join(stuck) if stuck else f"review round {rounds + 1} next"), stage=S.REVIEW
                )
            else:
                self.hand_off(", ".join(what), f"Round {rounds}: " + ", ".join(what), ours=not swept)
        else:
            self.reviewed_round = not unreviewed
            return looked
        return None
