"""Conary's `cn`, as the steward asks it: each PR's class (`cn class`, its
docs/DESIGN.md, section 4.4), and the merge train.

The merge train, in a repo with `train` on: Conary's `cn train` (its
docs/DESIGN.md, section 4.7), on the ledger's own log. The steward puts each
PR it settles in the repo's train instead of merging it (one_pr.py), takes
it out once it isn't settled any more, and steps the train once the repo's
PRs are seen to. cn does the rest as the steward's App: it merges the PRs in
the train onto the default branch together, a candidate, has the repo's CI
run on that on its `conary/train/` branch, lands a green one's PRs in order
through their own merges, and splits a red one to find the PR that broke
it. On each head it lands it posts its status (CONTEXT) first, which the
repo's rules require; one red alone gets a failure, which a fix job takes,
and one GitHub refused an error.

cn asks this very command for the App's tokens (`autoland-steward token
OWNER/REPO`, steward.py), one for each repo it calls: printed into cn's
pipe, never written anywhere.

Python, not Rust, only because the steward still is: the train itself is
Conary's, in Rust, and this is the steward's thin end of it, to go with the
rest of the steward when that's ported."""

import json
import os
import subprocess
import sys

import common as C
import stages as S
import waits
from common import CFG, DRY, log, repo_cfg

CONARY = CFG["conary"]
CONTEXT = CFG["trainContext"]
# Why the train holds a PR out, as cn says it: until what, in words, and
# the stage that leaves it at (stages.py). Never the train's own, whose label
# is Renovate's stop label: one held out is free for its bot to rebase.
HELD = {"red": ("it was red alone in it, until a new head", S.FIX),
        "conflict": ("it won't merge after the PRs ahead of it, until they've landed or left", S.BASE),
        # Its merge changes nothing after them: once they've landed, it's
        # closed (step.py's mergeable), or left to its dependency bot.
        "empty": ("it changes nothing after the PRs ahead of it, until they've landed or left", S.BASE),
        "refused": ("{}, until the default branch moves or a new head", S.BASE),
        "elsewhere": ("it was just retargeted off the default branch, so it's seen to again", S.REVIEW),
        # Why its negative (the repo's trainNegative) doesn't count, as cn
        # says it, its head's id cut short: its CI runs it.
        "unproven": ("{}, until a run of it on this head counts", S.CI)}
TRAINS = {}  # each train repo's train, as this pass read it (begin)
# The train repos this pass mustn't step, and why: a PR in the train it
# couldn't take out, or wasn't sure was still settled.
STOPPED = {}
CLASSES = {}  # each PR's class at a head, as cn said this pass (classed)


def on(repo):
    return bool(repo_cfg(repo)["train"])


def rides(repo, pr, info):
    """Whether the PR lands through the repo's train once it's settled: the
    train is on and the PR targets the default branch, the one the train
    lands onto. One against another branch merges as it would without it."""
    return on(repo) and pr["baseRefName"] == info["defaultBranchRef"]["name"]


def cn(*args):
    """What cn answered, as JSON. GitHub unavailable (cn's exit 75): C.Unavailable."""
    me = os.path.join(os.path.dirname(os.path.abspath(__file__)), "steward.py")
    env = {**os.environ, **CONARY["env"], "CONARY_TOKEN_COMMAND": f"{sys.executable} -E -s {me} token"}
    p = subprocess.run([CONARY["cn"], *args], env=env, capture_output=True, text=True)
    what = f"cn {' '.join(args[:3])}"
    if p.returncode == 75:
        raise C.Unavailable(f"{what}: GitHub is unavailable")
    if p.returncode:
        raise RuntimeError(f"{what}: {p.stderr.strip()[-300:]}")
    return json.loads(p.stdout)


def classed(repo, pr):
    """The PR's class at its head, as Conary's class policy says (`cn
    class`, which records it): its class, what made it R2 (`because`), and
    whether it needs a review round (`review`: anything but documentation,
    and documentation that's R2). Asked once a pass, and only when a
    decision turns on it. A dry run asks nothing, since cn would record the
    class, and takes every PR for R2."""
    n, head = pr["number"], pr["headRefOid"]
    if (repo, n, head) not in CLASSES:
        if DRY:
            CLASSES[repo, n, head] = {"class": "R2", "because": "a path a dry run doesn't class", "review": True}
        else:
            CLASSES[repo, n, head] = cn("class", repo, str(n), "--head", head)
    return CLASSES[repo, n, head]


def raised(repo, pr):
    """The raise labels Conary holds for the PR at its head (`cn class`'s
    raised): `r2` and `review`, its author's ask, which taking the label
    off doesn't undo."""
    return classed(repo, pr).get("raised", [])


def gate(repo, pr):
    """What makes the PR R2 at its head: the path that does, or why its
    paths couldn't all be read. None below R2."""
    got = classed(repo, pr)
    return (got["because"] or "an R2 path") if got["class"] == "R2" else None


def review(repo, pr):
    """Whether the PR needs a review round at its head: documentation alone
    that isn't R2 needs green CI and nothing more (Conary's docs/DESIGN.md,
    section 4.4)."""
    return bool(classed(repo, pr)["review"])


def begin(repo):
    """The repo's train, as the ledger holds it, for this pass: none with
    its train off, whatever an earlier pass read. Its PRs' classes are asked
    afresh."""
    TRAINS.pop(repo, None)
    STOPPED.pop(repo, None)
    for k in [k for k in CLASSES if k[0] == repo]:
        del CLASSES[k]
    if on(repo):
        TRAINS[repo] = cn("train", "show", repo)


def short(candidate):
    return candidate[:8]


def seat(repo, n):
    """Where PR n rides the repo's train, in words, as this pass read it:
    None when it isn't in it. What it names (the candidate in flight, its
    own or the one ahead of it) is what it waits on."""
    t = TRAINS.get(repo)
    if not t or not any(m["pr"] == n for m in t["joined"]):
        return None
    flight = next((c for c in t["candidates"] if c["candidate"] == t["in_flight"]), None)
    # The PRs that joined before it, in flight or not: the train takes them
    # in the order they joined.
    ahead = next(i for i, m in enumerate(t["joined"]) if m["pr"] == n)
    if flight is None:
        return f"{ahead} ahead of it" if ahead else "next in line"
    with_ = " ".join(f"#{p}" for p in flight["prs"] if p != n)
    if n not in flight["prs"]:
        return f"behind candidate {short(flight['candidate'])}, {ahead} ahead of it"
    doing = "landing" if flight["state"]["state"] == "green" else "gating"
    return f"{doing} in candidate {short(flight['candidate'])}" + (f" with {with_}" if with_ else "")


def join(repo, pr, reviewed):
    """Put the PR in the train at its head. None once it's in, else why the
    train holds it out (a key of HELD) and what cn said of it, if anything. cn
    classes it by the machine's policy, as `cn class` does: an R2 one rides
    in a candidate of its own, which lands only while the default branch is
    still its base. It names the raise labels this pass settled it with
    (`cn class`'s raised): one cn finds that this pass didn't settle it
    with holds it out, for the next pass to settle it with. In a repo with
    a trainNegative, only once its negative on that head counts. One that
    had no review round (a dependency update's, whose review is its CI)
    wasn't settled with a `review` raise, so it doesn't name one."""
    n, head = pr["number"], pr["headRefOid"]
    negative = repo_cfg(repo)["trainNegative"]
    if DRY:
        log(f"  would: put {repo}#{n} in its train at {head[:7]}")
    else:
        settled = [a for label in raised(repo, pr) if reviewed or label != "review" for a in ("--raised", label)]
        got = cn("train", "join", repo, str(n), "--head", head, *settled,
                 *(["--negative", negative] if negative else []))
        if got.get("unproven"):
            return "unproven", got["unproven"].replace(head, head[:7])
        if got.get("held"):
            return got["held"], None
        if got.get("targets"):
            return "elsewhere", None
        if got["joined"]:
            log(f"{repo}#{n}: joined its train at {head[:7]}")
    t = TRAINS.setdefault(repo, {"joined": [], "in_flight": None, "candidates": []})
    if not any(m["pr"] == n for m in t["joined"]):
        t["joined"].append({"pr": n})
    return None


def ride(repo, pr, st, desc, settle, strike, said, reviewed):
    """A PR settled against the default branch: into the train at this head,
    which lands it, rather than merged by the steward (alone if it's R2,
    which cn says). Its status says where in the train it is, or why the train
    holds it out and until what. Nothing else may merge it: a merge queued on
    it is taken off, or it's tried again (strike). st is the PR's state, which
    remembers the head it joined at; settle and strike are its step's own
    (step.py); said is what GitHub said when it refused its merge, as the
    train's status on the head has it; reviewed, whether its reviewers had
    a round on it (join)."""
    n, head = pr["number"], pr["headRefOid"]
    if C.queued(pr) and not C.unland(repo, pr):
        strike("status")
        settle("pending", "Settled, but GitHub didn't take its queued merge off; trying again", stage=S.MERGE)
        return
    where = seat(repo, n) if st.get("joined") == head else None
    if where is None:
        held = join(repo, pr, reviewed)
        if held:
            held, why = held
            until, stage = HELD.get(held, (held, S.BASE))
            said = until.format(why or said or "GitHub refused its merge")
            settle("pending", f"Settled, but held out of the train: {said}", stage=stage, wait=waits.TRAIN)
            return
        st["joined"] = head
        where = seat(repo, n)
    # Its stage's label is Renovate's stopUpdatingLabel too (updates.nix): a
    # new head on a PR in a candidate throws the candidate away, gate and
    # all, and one behind it would be next.
    if not settle("success", f"In the train, {where}. {desc}"[:140], stage=S.TRAIN, wait=waits.TRAIN) and not DRY:
        # Without its status cn wouldn't land it (Conary#55): out until
        # GitHub takes it (settle strikes it).
        leave(repo, pr)


def leave(repo, pr):
    """Take the PR out of the train: it isn't settled any more. Not while
    its candidate is landing a PR, which a step settles first: it's out on
    a pass after that, its status saying it isn't settled meanwhile, which
    cn reads right before each landing, taking it out instead (Conary#55).
    Returns whether it's out."""
    n = pr["number"]
    if DRY:
        log(f"  would: take {repo}#{n} out of its train")
    else:
        got = cn("train", "leave", repo, str(n), "--why", "unready")
        if got.get("landing"):
            log(f"{repo}#{n}: still in its train, unsettled: #{got['landing']} is landing in its candidate")
            return False
        log(f"{repo}#{n}: left its train, unsettled")
    t = TRAINS[repo]
    t["joined"] = [m for m in t["joined"] if m["pr"] != n]
    return True


def stop(repo, why):
    """No step of the repo's train this pass: what it holds may not all be
    settled, and its old status, which cn reads, still says it is."""
    STOPPED.setdefault(repo, why)


def idle(t):
    """Nothing for a step to do: nothing in the train or in flight, and the
    last candidate finished (the step that finished it cleaned up)."""
    last = t["candidates"][-1]["state"]["state"] if t["candidates"] else "finished"
    return not t["joined"] and t["in_flight"] is None and last == "finished"


def step(repo):
    """One step of the repo's train, once its PRs are seen to: it builds,
    gates and lands (cn's doc says how far), onto the default branch as
    GitHub says it is. What it did shows on the PRs'
    heads (the train's status), and each change it makes on GitHub is an
    event that starts the repo's next pass."""
    t = TRAINS.get(repo)
    if t is None or idle(t):
        return
    if repo in STOPPED:
        # The PR's own pass, booked later, steps it.
        log(f"{repo}: its train isn't stepped this pass: {STOPPED[repo]}")
        return
    rc = repo_cfg(repo)
    args = ["train", "step", repo, "--size", str(rc["trainSize"])]
    for check in rc["trainRequire"]:
        args += ["--require", check]
    if DRY:
        log(f"  would: step {repo}'s train")
        return
    got = cn(*args)
    said = [f"candidate {short(c)} {v}" for c, v in got["verdicts"]]
    said += [f"#{x['pr']} red alone" for x in got["red"]]
    said += [f"#{x['pr']} won't merge" for x in got["conflicts"]]
    said += [f"#{x['pr']} refused: {x['why']}" for x in got["refused"]]
    said += [f"#{n} merged" for n in got["landed"]]
    said += [f"#{n} out, no longer settled" for n in got["unready"]]
    said += [f"#{n} back in alone, R2 since it joined" for n in got["reseated"]]
    now = got["now"]
    if now["state"] == "gating":
        said.append(f"candidate {short(now['candidate'])} gating on {now['branch']}"
                    + (f", waiting on {', '.join(now['waiting'])}" if now["waiting"] else ""))
    elif now["state"] == "landing":
        said.append(f"candidate {short(now['candidate'])} landing")
    log(f"{repo}: train: {'; '.join(said) or 'idle'}")
