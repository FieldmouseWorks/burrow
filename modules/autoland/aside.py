"""Why a PR is set aside before anything else is done with it (step.py):
nobody reviews it, no Claude job works on it and nothing merges it, and its
status says why. A draft, an outside contribution, a PR with more on it than
one page shows, a red-branch fix held, a hold, or a PR stacked on another
PR's branch."""

import common as C
import stages as S
import waits
from common import CFG, log


def aside(tag, pr, labels, held, below):
    """(status, why, stage, wait) for a PR set aside, as step.py's settle
    takes them (why: the owner's digest heading, or None), or None for one
    that isn't.
    below is the number of the open PR whose head branch is this one's base,
    if there is one."""
    if pr["isDraft"]:
        # Work in progress, whoever's it is: nobody reviews it yet, and nobody
        # is told.
        return "Draft: work in progress, reviewed once it's marked ready", None, S.NONE, None
    if not C.acted_on(pr):
        return f"Outside contribution: {CFG['owner']} reviews and merges it", "Outside contributions", S.OWNER, waits.STEWARD
    # A list longer than one page could hide a hold, an open thread, a
    # changes-requested review or a verdict: fail closed. (open_prs reads
    # older reviews and comments, so those two are cut only past its cap.)
    cut = [k for k, more in (("labels", pr["labels"]["pageInfo"]["hasNextPage"]),
                             ("review threads", pr["reviewThreads"]["pageInfo"]["hasNextPage"]),
                             ("reviews", pr["reviews"]["pageInfo"]["hasPreviousPage"]),
                             ("comments", pr["comments"]["pageInfo"]["hasPreviousPage"])) if more]
    if cut:
        return f"Too many {' and '.join(cut)} to read: {CFG['owner']} merges it", "Too much to read", S.OWNER, waits.STEWARD
    if held:
        # A fix for a red default branch that mustn't land now
        # (R.watch_default): the branch isn't red any more, or its CI hasn't
        # said yet whether it still is. Like a hold, nothing may merge it.
        return held[:140], None, S.BASE, waits.STEWARD
    if CFG["labels"]["hold"] in labels:
        # Held after it was settled: take back the status and the queued merge.
        log(f"{tag}: on hold")
        return f"On hold (the {CFG['labels']['hold']} label)", None, S.NONE, waits.HOLD
    if below:
        # Stacked on another open PR's branch: merged now, it would land in
        # that branch, folded into the PR below, and its review and CI judge
        # its change against a base that's still moving. Its turn comes once
        # it's retargeted (GitHub does that itself when the PR below merges
        # and its branch is deleted), which is a new base, so it's reviewed
        # then. A PR into a branch no open PR has (a release branch) isn't
        # stacked. Nobody waits on the pipeline for it until then: the PR
        # below is watched.
        return f"Stacked on #{below} ({pr['baseRefName']}): reviewed and landed once it's retargeted"[:140], None, S.BASE, None
    return None
