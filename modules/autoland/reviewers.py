"""The steward's reviewer: Codex, through its GitHub connector (the ChatGPT
Codex Connector App, set up on the owner's ChatGPT account to review every
push). It reviews each head on its own and posts on the PR: findings as a
review with inline threads, and a summary comment naming the commit it last
reviewed and whether it found anything. The steward reads its verdict from
those (`verdict`, burrow-upkeep's, crates/burrow-upkeep/src/verdict.rs).

When a look is owed and no push will bring it, the steward asks for one
(`ask`). The connector takes that ask only from the owner: an App's gets no
review. So the steward never posts it itself: the owner's door,
burrow-review-ask (crates/burrow-review-ask), posts the bare ask as the
machine user's own gh login, in a unit of that user's own, in the repos the
machine lists (askAsOwner), once a head; the steward only reads its record
(`asked`)."""

import json
import subprocess

import common as C
import prs as P
from common import CFG, DRY, log

NAMES = {"codex": "Codex"}


def on(repo):
    """The reviewers this repo has: Codex, unless the repo turned it off."""
    rc = C.repo_cfg(repo)
    return [r for r in NAMES if rc[r]]


def verdict(repo, pr):
    """What Codex last said of the PR's head (none, running, clean or
    findings), when, how many of its reviews of the PR had findings, and
    whether the PR was retargeted or its description edited after its head
    was pushed, which Codex's word doesn't cover (burrow-upkeep's `verdict`
    says why), from the connector's own posts. Asks GitHub nothing; raises
    when it couldn't tell."""
    retargets = (pr.get("retargets") or {}).get("nodes") or [{}]
    out = C.upkeep("verdict", repo, as_app=False, input=json.dumps({
        "head": pr["headRefOid"], "reviews": pr["reviews"]["nodes"], "comments": pr["comments"]["nodes"],
        "edited": pr.get("lastEditedAt"), "retargeted": retargets[-1].get("createdAt"), "pushed": P.pushed(pr)}))
    if out["said"] is None:
        raise RuntimeError(out["why"])
    s = out["said"]
    return s["verdict"], s["at"], s["rounds"], s["changed"]


def door(repo):
    """Whether the owner's door may ask in this repo (askAsOwner)."""
    d = CFG.get("askAsOwner")
    return bool(d) and repo.lower() in (r.lower() for r in d["repos"])


def asked(repo, pr, said):
    """When the owner's door asked for a review of the PR's head with
    Codex's word on it as it is (said: when Codex last said anything of the
    head, 0 for nothing), from the door's own record, or None."""
    if not CFG.get("askAsOwner"):
        return None
    try:
        record = json.load(open(CFG["askAsOwner"]["record"]))["asked"]
    except (OSError, ValueError, KeyError, TypeError):
        return None
    head = pr["headRefOid"]
    return next((a["at"] for a in record if a["repo"].lower() == repo.lower() and a["pr"] == pr["number"]
                 and a["head"] == head and a["said"] == int(said)), None)


def ask(repo, pr):
    """Have the owner's door ask the connector to review the PR's head:
    (when it was asked, or None; why not, or None; what failed, or None).
    A door that failed (GitHub refused the comment, or its broker didn't
    answer) is the caller's to strike; one whose checks said no isn't."""
    if not door(repo):
        return None, "this repo isn't one the owner's door asks in", None
    n, head = pr["number"], pr["headRefOid"]
    if DRY:
        log(f"  would: ask Codex as the owner to review {repo}#{n} at {head[:7]}")
        return None, None, None
    out = ask_as_owner(repo, n, head)
    what = out.get("outcome")
    if what in ("posted", "asked"):
        if what == "posted":
            log(f"{repo}#{n}: asked Codex, as the owner, to review {head[:7]}")
        return out["at"], None, None
    if what == "declined":
        return None, out["why"], None
    return None, None, out.get("why") or "the owner's door said nothing"


def ask_as_owner(repo, n, head):
    """The owner's door (burrow-review-ask, config.askAsOwner) on the PR's
    head: what came of it, as it says (its Outcome). Its command asks the
    door's broker (autoland/review-ask.nix), as conaryd's flow does too,
    which runs it with the machine user's gh login while this unit never
    holds it, and nothing of this unit's environment (its App token) goes
    with it. Waited for: it reads the PR and posts at most
    one comment."""
    try:
        p = subprocess.run([*CFG["askAsOwner"]["command"], repo, str(n), head],
                           stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=300)
    except (OSError, subprocess.TimeoutExpired) as e:
        return {"outcome": "failed", "why": f"the owner's door: {e}"}
    try:
        return json.loads(p.stdout.strip().splitlines()[-1])
    except (IndexError, ValueError):
        return {"outcome": "failed", "why": f"the owner's door: {p.stderr.strip()[-300:] or p.returncode}"}
