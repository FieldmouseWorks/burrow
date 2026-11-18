"""The reviewer's part of one PR's step (step.py): what Codex's GitHub
connector has said of its head (reviewers.py), and what that leaves the PR
waiting on. step.py then decides on it, with its threads, its CI and its
gates."""

from typing import NamedTuple

import common as C
import reviewers as V
from common import MIN, repo_cfg

# A push the connector hasn't begun on after this long gets asked for once,
# and after LOST the owner hears of it: it reviews each push within minutes
# when its usage lasts.
QUIET, LOST = 30, 120


class Looked(NamedTuple):
    """What the PR's reviewers come to on its head, for its step."""
    live: list  # the reviewers this repo has
    said: dict  # each one's verdict on this head: (verdict, when, rounds with findings)
    rounds: int  # the most rounds with findings any reviewer has
    clean: bool  # every one found nothing on this head
    capped: bool  # past the repo's round limit, and not clean
    waiting: list  # what it still waits on from the reviewers, in words
    stuck: list  # what the owner hears of, in words
    unreviewed: bool  # it needs no review round: documentation alone, not R2


def look(repo, pr, st, digest, line, age, after, needed):
    """The reviewers' part of the PR's step this pass. st is its state;
    digest and line its listing; age how long its head has been still
    (since its push, as near as GitHub says); after books its next pass
    (step.py's); needed, whether its head needs a review round at all
    (train.review)."""
    rc = repo_cfg(repo)
    names = V.on(repo)
    said = {r: V.verdict(repo, pr) for r in names}
    rounds = max((said[r][2] for r in names), default=0)
    clean = bool(names) and all(said[r][0] == "clean" for r in names)
    # Past the limit the steward stops waiting on the reviewers: a Claude
    # job settles what's left on its merits, and a judge decides whether it
    # lands. The connector still reviews each push; it just isn't waited for.
    capped = rounds >= rc["rounds"] and not clean
    # Documentation alone that isn't R2 needs green CI and no review round
    # (Conary's class, asked only when the reviewer hasn't settled it).
    unreviewed = not clean and not needed()
    if unreviewed:
        clean, capped = True, False
    waiting, stuck = [], []
    if not names and not unreviewed:
        waiting.append("No reviewer can review it (Codex is off for this repo)")
        digest.append(("No reviewer can review it", line, "Codex is off for this repo"))
    for r in [] if clean or capped else names:
        v = said[r][0]
        if v == "running":
            waiting.append(f"{V.NAMES[r]} is reviewing this commit")
        elif v == "none":
            waiting.append(f"Waiting for {V.NAMES[r]}'s review of this commit")
            if age >= QUIET * MIN:
                V.ask(repo, pr, st, f"No review of {pr['headRefOid'][:7]} came after its push.")
            else:
                after(C.NOW - age, QUIET)  # ends: asked once, then listed
            if age >= LOST * MIN:
                x = f"no review of {pr['headRefOid'][:7]} after {int(age // MIN)} minutes"
                stuck.append(f"{V.NAMES[r]}: {x}")
                digest.append((f"{V.NAMES[r]} hasn't reviewed it", line, x + " (its usage, or the connector's setup)"))
            else:
                after(C.NOW - age, LOST)  # ends: listed for the owner
    return Looked(names, said, rounds, clean, capped, waiting, stuck, unreviewed)
