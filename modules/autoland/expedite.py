"""The expedited step: a repo's pass told its events were about PRs that go
first (labels.first, by an author: firsts.py) steps those PRs then and there,
with step.handle as every pass does, and leaves the repo's other PRs, their
state, wake-ups, digest lines and place in the proof line, to its ordinary
passes.

burrow-events tells a repo's pass what its events were about (its
`prTargets`): `pr N`, `branch NAME` or `commit SHA`, a line each, in the file
BURROW_EVENTS_PRS names while the pass runs. A pass told only PRs that go
first is expedited; one told anything else (an ordinary PR, a branch no open
PR has, a commit that's no PR's head) is the repo's ordinary pass, which takes
first PRs first anyway. burrow-events starts a first PR's pass ahead of every
other: it lists them from FILE (`firsts`, written here by every pass that
reads a repo's PRs), and while one waits its marker (CFG firstMarker) is
there, so a pass already running yields at its next PR (batch.py) and a
continuation waits (steward.py). Neither runs a step of its own: there's one
runner of steps, and one pass at a time holds the pass lock.

Nothing here skips a gate: step.handle holds, pauses, reviews, waits for CI,
judges and lands as it always does. A red default branch's held fixes are
stepped with it, as every pass steps them first. In a repo with its merge
train on, a PR it seats in the train books the repo's ordinary pass, which
steps the train once it has seen every PR."""

import os
import time

import common as C
from common import CFG

FILE = "firsts.json"
# A marker older than this is left over from something gone (burrow-events
# stopped without its runtime directory going): nothing waits on it.
MARKER_FRESH = 10 * C.MIN


def told(path=None):
    """What this pass was told its events were about: a set of (kind, value),
    kind "pr", "branch" or "commit"; None for a pass that looks at
    everything (not told, or told something it can't read)."""
    path = path if path is not None else os.environ.get("BURROW_EVENTS_PRS")
    if not path:
        return None
    try:
        with open(path) as f:
            lines = [x.strip() for x in f if x.strip()]
    except OSError:
        return None
    got = set()
    for line in lines:
        kind, _, value = line.partition(" ")
        if kind == "pr" and value.isdigit():
            got.add(("pr", int(value)))
        elif kind in ("branch", "commit") and value and " " not in value:
            got.add((kind, value))
        else:
            return None
    return got or None


def lines(about):
    """told's set as the lines a kick carries (burrow-events' told.rs)."""
    return "".join(f"{k} {v}\n" for k, v in sorted(about, key=str))


def resolve(about, prs):
    """The open PRs each of `about` names, by number: a PR by its number, a
    branch by the PRs whose head it is (in this repo), a commit by those
    whose head it is. None if any names none: a closed PR, the default
    branch, a train's commit."""
    out = set()
    for kind, value in about:
        if kind == "pr":
            got = {pr["number"] for pr in prs if pr["number"] == value}
        elif kind == "branch":
            got = {pr["number"] for pr in prs if pr.get("headRefName") == value and not pr.get("isCrossRepository")}
        else:
            got = {pr["number"] for pr in prs if (pr.get("headRefOid") or "").lower() == value.lower()}
        if not got:
            return None
        out |= got
    return out


def picked(kick, about, prs, first):
    """The PRs an expedited pass steps, in the order it steps them; None for
    the repo's ordinary pass. Only a kicked pass over one repo, told only
    PRs that go first, is expedited. (Once the expedite lane's active item
    names its PRs, only those will be: #954.)"""
    if kick is None or len(kick) != 1 or not about:
        return None
    numbers = resolve(about, prs)
    if numbers is None or not numbers <= set(first):
        return None
    return [pr for pr in prs if pr["number"] in numbers]


def remember(repo, prs, first):
    """FILE: the repo's first PRs as this pass read them, for burrow-events
    to put their events first; the other repos' as they were."""
    if C.DRY:
        return
    was = C.load(FILE, {})
    got = {k: v for k, v in was.items() if k.lower() != repo.lower()}
    mine = [{"number": pr["number"], "branch": None if pr.get("isCrossRepository") else pr.get("headRefName"),
             "head": pr.get("headRefOid")} for pr in prs if pr["number"] in first]
    if mine:
        got[repo] = mine
    if got != was:
        C.save(FILE, got)


def waiting():
    """Whether a pass for a first PR waits to start (its marker is there)."""
    marker = CFG.get("firstMarker")
    if not marker:
        return False
    try:
        return time.time() - os.stat(marker).st_mtime < MARKER_FRESH
    except OSError:
        return False


def about(repo, number):
    """The start of a digest line's link to the PR (step_context's line)."""
    return f"[{repo}#{number}]("


def keep(repo, mine, numbers):
    """The repo's digest lines with those about these PRs taken out: what
    their step says goes back in, and every other line stays."""
    marks = tuple(about(repo, n) for n in numbers)
    return [x for x in mine if not x[1].startswith(marks)]
