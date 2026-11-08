"""Dependency updates: keep sweep ownership and major verdicts ahead of
review, proofs and landing. step.py supplies the same reviewer step used
for ordinary PRs, and the actions that own its state and status.
"""

import time

import common as C
import stages as S
import train as T
import waits
from common import CFG
from seal import MAJOR as MAJOR_VERDICT
from tasks import DEP_SEEN, PAST_LIMIT, REVIEWED
from touches import automerge_spent, automerges, provenance


def step(repo, pr, st, *, dep, renovate, head, labels, refresh, open_threads, changes,
         ci, ci_names, held, proof_only, build_early, asked, gate, reviewed,
         settle, hand_off, judged, prove, land_now, mergeable, clock):
    looked = None
    major_verdict = C.marker(pr, MAJOR_VERDICT, head, repo, pr["baseRefName"]) if dep == "major" and renovate else None
    threads = f"{open_threads} open review thread{'s' * (open_threads > 1)}" if open_threads else ""
    if changes:
        threads = ", ".join([threads] * bool(threads) + ["changes requested by " + ", ".join(changes)])
    if (ripe := refresh.get("ripe")) and not threads:
        when = time.strftime("%b %-d, ", time.localtime(ripe)) + clock(ripe)
        settle("pending", f"Lock file refresh, nothing in it old enough until {when}; renovate-sweep has it refreshed then",
               stage=S.RELEASE_AGE, wait=waits.SWEEP)
    elif renovate and ci == "red":
        settle("pending", "Dependency update, CI red; renovate-sweep has it" + (f" ({threads})" if threads else ""),
               stage=S.FIX, wait=waits.SWEEP)
    elif renovate and dep == "major" and threads and not major_verdict:
        # Until it gives this head a verdict, renovate-sweep's job has its
        # branch: one job on it. Its threads and reviews wait for that
        # verdict, then go to a job of ours.
        settle("pending", f"Major update, {threads}; renovate-sweep has it"[:140], stage=S.FIX, wait=waits.SWEEP)
    elif renovate and dep == "major" and major_verdict == "held":
        # Held for the owner (renovate-sweep tells them): no job pushes
        # to it, threads or not, and changes what they're deciding on.
        settle("pending", "Major update; renovate-sweep held it: see its comment", stage=S.OWNER, wait=None)
    elif threads or ci == "red":
        what = threads or "CI is red (" + ", ".join(ci_names) + ")"
        kind = "handed" if threads else "redFixed"
        hand_off(what, f"Dependency update; {what}", kind)
    elif dep == "major" and renovate and major_verdict != "ok":
        # renovate-sweep's verdict comes first, and its job runs the proofs
        # until then: neither a judge nor a proof job of the steward's
        # works the branch beside it.
        settle("pending", "Major update; waiting for renovate-sweep's verdict on this head", stage=S.REVIEW, wait=waits.SWEEP)
    elif "review" in T.raised(repo, pr) and not (looked := reviewed()):
        pass  # the shared reviewer step posted its wait or handed off findings
    elif held:
        judged("a Claude job was asked to judge this head", DEP_SEEN)
    elif proof_only or build_early:
        prove()
    elif ci == "pending":
        settle("pending", "Dependency update, CI pending", stage=S.CI)
    elif looked and (looked.unseen or looked.capped):
        judged("reviewed dependency update with unseen paths or past the review-round limit",
               PAST_LIMIT if looked.capped else REVIEWED)
    elif gate() or ci == "none" or asked:
        judged(f"a dependency update touching {gate()}" if gate() else "the repo has no CI" if ci == "none"
               else "a Claude job was asked to judge this head",
               DEP_SEEN + ("; renovate-sweep cleared the major version for this head" if major_verdict == "ok" else ""))
    elif dep == "major" and not renovate:
        judged("a major dependency update", DEP_SEEN)
    elif dep == "major":
        land_now("Major update renovate-sweep cleared, CI green")
    elif renovate and not T.on(repo) and not ((automerge_spent(pr) or automerges(pr) and CFG["labels"]["batch"] in labels)
                                              and provenance(repo, pr, st) is False):
        # Renovate merges its own, on its next run on the repo, except in
        # a repo with its train on, where it merges none (updates.nix):
        # the train lands those against the default branch, and land_now
        # the rest. So does a batch of a trusted owner's branch pins (its
        # label): Renovate merges none while that branch keeps moving
        # (updates/renovate-policy.nix), but only one whose commits were
        # read and are Renovate's alone (provenance): Renovate checks
        # that before it merges, so one that couldn't be read waits for
        # it. So does one whose automerge Renovate switched off itself
        # (automerge_spent), which it never merges. One with it on, open past
        # renovateMergeAfter is a stall, which that run is kicked for
        # first (waits.py). Not one nothing may merge (mergeable): one
        # whose change has landed already is Renovate's to close.
        if mergeable():
            settle("success", "Dependency update, CI green", stage=S.MERGE,
                   wait=waits.RENOVATE if automerges(pr) else waits.STEWARD)
    else:
        land_now("Dependency update, CI green")
