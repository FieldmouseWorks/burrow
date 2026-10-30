"""A repo's proofSlots: how many of its open PRs may have a proof job queued
or running at once, for proofs that take the whole machine (the rest would
only wait out their timeout). The others wait their turn, the PRs labelled
`first` (labels.first) ahead of the rest and the oldest first within each,
and the end of a job's turn kicks the repo's pass, which starts the next.

What a slot is held by is the proof jobs themselves, by id, whatever head
they were started on: a PR pushed again, or marked a draft, while its job
runs still has it. A turn in the line is only this pass's: a PR is ahead
while it's waiting in the same pass (they go in their line's order, place),
so one held, drafted or done since holds nobody up."""

import os
import re

import common as C
from common import STATE, load

OPEN = {}  # the open PRs of each repo this pass reads (begin)
FIRST = {}  # those of them labelled to go first
WAITING = {}  # the PRs of each repo this pass found waiting their turn


def begin(repo, numbers, first=()):
    """A pass over the repo's open PRs starts: only theirs count, and those
    in first go ahead of the rest."""
    OPEN[repo] = set(numbers)
    FIRST[repo] = set(first)
    WAITING[repo] = set()


def place(repo, n):
    """Where PR n stands in its repo's line this pass: one labelled first
    ahead of every one that isn't, and the oldest first within each. The
    pass takes the PRs in this order, so a slot goes to the first in line."""
    return n not in FIRST.get(repo, ()), n


def jobs(st):
    """A PR's proof jobs that may still hold a slot: every one started since
    the last look (proofsLive), and its latest (from before there was one)."""
    latest = [st["provedJob"]] if st.get("provedJob") else []
    return list(dict.fromkeys(st.get("proofsLive", []) + latest))


def running(jid):
    return C.job_state(jid) in (None, "queued", "running")


def live(st):
    """The PR's proof jobs still queued or running, or that proj can't say
    about: kept in its state, the ended ones dropped."""
    st["proofsLive"] = [j for j in jobs(st) if running(j)]
    return st["proofsLive"]


def ahead(repo, n, st, slots):
    """How many proofs are ahead of PR n's: every open PR's live jobs, its
    own included, and the PRs ahead of it in line waiting their turn this pass.
    None when its job may start now."""
    busy = len(live(st))
    mine = re.escape(f"pr-{repo.replace('/', '_')}-") + r"(\d+)\.json"
    for f in os.listdir(STATE):
        m = re.fullmatch(mine, f)
        if m and int(m[1]) != n and (repo not in OPEN or int(m[1]) in OPEN[repo]):
            busy += sum(map(running, jobs(load(f, {}))))
    waiting = sum(place(repo, other) < place(repo, n) for other in WAITING.get(repo, ()))
    return busy + waiting if busy + waiting >= slots else None


def wait(repo, n):
    """PR n waits its turn this pass: the ones after it wait behind it."""
    WAITING.setdefault(repo, set()).add(n)


def started(st):
    """The PR's latest proof job, just started (or not): it holds a slot
    until it ends, whatever job starts after it."""
    st["proofsLive"] = jobs(st)
