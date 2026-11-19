"""The reviewer's part of one PR's step (step.py): what Codex's GitHub
connector has said of its head (reviewers.py), and what that leaves the PR
waiting on. step.py then decides on it, with its threads, its CI and its
gates."""

from typing import NamedTuple

import common as C
import reviewers as V
from common import MIN, repo_cfg
from step_context import clock

# A push the connector hasn't begun on after this long gets asked for once,
# through the owner's door (reviewers.ask); after LOST the owner hears of
# it, and once asked, only when the ask too went ANSWERED minutes without a
# word: it reviews each push within minutes when its usage lasts, and
# answers the owner's ask within a minute.
QUIET, LOST, ANSWERED = 30, 120, 30


class Looked(NamedTuple):
    """What the PR's reviewers come to on its head, for its step."""
    live: list  # the reviewers this repo has
    said: dict  # each one's verdict on this head: (verdict, when, rounds with findings, changed)
    rounds: int  # the most rounds with findings any reviewer has
    clean: bool  # every one found nothing on this head
    capped: bool  # past the repo's round limit, and not clean
    waiting: list  # what it still waits on from the reviewers, in words
    stuck: list  # what the owner hears of, in words
    unreviewed: bool  # it needs no review round: documentation alone, not R2
    changed: bool  # retargeted or edited after its push: clean isn't the reviewers' to settle alone


def look(repo, pr, digest, line, age, after, tried, needed):
    """The reviewers' part of the PR's step this pass. digest and line are
    its listing; age how long its head has been still (since its push, as
    near as GitHub says); after books its next pass, and tried strikes the
    ask when the owner's door failed or clears it (step.py's); needed,
    whether its head needs a review round at all (train.review)."""
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
            at, unasked = V.asked(repo, pr, 0), None
            if at is None and age >= QUIET * MIN:
                at, unasked, failed = V.ask(repo, pr)
                tried("ask", failed)
                unasked = unasked or failed
            elif at is None:
                after(C.NOW - age, QUIET)  # ends: asked once, then listed
            # The owner hears of it once its push is LOST minutes old and,
            # when the owner's door asked, the ask went unanswered too.
            due = max(C.NOW - age + LOST * MIN, (at or 0) + ANSWERED * MIN)
            if C.NOW >= due:
                x = f"no review of {pr['headRefOid'][:7]} after {int(age // MIN)} minutes"
                how = f"asked as the owner at {clock(at)}, unanswered" if at else f"not asked as the owner: {unasked}" if unasked else "not asked"
                stuck.append(f"{V.NAMES[r]}: {x} ({how})")
                digest.append((f"{V.NAMES[r]} hasn't reviewed it", line, f"{x}; {how} (its usage, or the connector's setup)"))
            else:
                after(due, 0)  # ends: listed for the owner
    # Retargeted or edited after its push, Codex's clean can't settle it: it
    # binds no base or description (burrow-upkeep's verdict), so a judge,
    # whose verdict binds both, decides (step.py).
    changed = clean and not unreviewed and any(said[r][3] for r in names)
    return Looked(names, said, rounds, clean, capped, waiting, stuck, unreviewed, changed)
