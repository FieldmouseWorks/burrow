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


def look(repo, pr, st, digest, line, age, after, tried, needed, replacing=False):
    """The reviewers' part of the PR's step this pass. st is its state;
    digest and line its listing; age how long its head has been still (since
    its push, as near as GitHub says); after books its next pass, and tried
    strikes the ask when the owner's door failed or clears it (step.py's); needed,
    whether its head needs a review round at all (train.review); replacing,
    whether a fix job that must push a new head over this one holds it (a
    conflict's): no review is asked of it, nor is its missing one listed,
    since what a review found would land on a head already on its way out,
    after the job read the PR (burrow#1324). Its push is asked about anew."""
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
        elif v == "none" and replacing:
            waiting.append(f"No {V.NAMES[r]} review asked of this commit: a Claude job is replacing it")
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
                # Since when, never for how long: the stuck issue knows an
                # entry by its words, and a count of minutes made the same
                # stall news on every pass (burrow#1228).
                # The status sits on the head already, and its 140 characters
                # must keep the ask's word, so it leaves the commit out.
                since = f"since its push at {clock(st['seen'])}"
                how = f"asked as the owner at {clock(at)}, unanswered" if at else f"not asked as the owner: {unasked}" if unasked else "not asked"
                stuck.append(f"{V.NAMES[r]}: no review {since} ({how})")
                x = f"no review of {pr['headRefOid'][:7]} {since}; {how}"
                digest.append((f"{V.NAMES[r]} hasn't reviewed it", line, x + " (its usage, or the connector's setup)"))
            else:
                after(due, 0)  # ends: listed for the owner
    # Retargeted or edited after its push, Codex's clean can't settle it: it
    # binds no base or description (burrow-upkeep's verdict), so a judge,
    # whose verdict binds both, decides (step.py).
    changed = clean and not unreviewed and any(said[r][3] for r in names)
    return Looked(names, said, rounds, clean, capped, waiting, stuck, unreviewed, changed)
