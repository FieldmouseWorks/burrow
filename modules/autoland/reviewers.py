"""The steward's reviewer: Codex, through its GitHub connector (the ChatGPT
Codex Connector App, set up on the owner's ChatGPT account to review every
push). It reviews each head on its own and posts on the PR: findings as a
review with inline threads, and a summary comment naming the commit it last
reviewed and whether it found anything. The steward reads its verdict from
those (`verdict`, burrow-upkeep's, crates/burrow-upkeep/src/verdict.rs) and
asks for a look only when one is owed and no push will bring it (`ask`)."""

import json

import common as C
from common import DRY, MIN, log

NAMES = {"codex": "Codex"}
# What asks the connector for a review, as its docs give it.
ASK = "@codex review"


def on(repo):
    """The reviewers this repo has: Codex, unless the repo turned it off."""
    rc = C.repo_cfg(repo)
    return [r for r in NAMES if rc[r]]


def verdict(repo, pr, st):
    """What Codex last said of the PR's head (none, running, clean or
    findings), when, and how many of its reviews of the PR had findings, by
    burrow-upkeep's `verdict`, from the connector's own posts: nothing from
    before the PR's last retarget or description edit counts. st, the PR's
    state, keeps when that was (its "since"), for ask. Asks GitHub nothing;
    raises when it couldn't tell."""
    retargets = (pr.get("retargets") or {}).get("nodes") or [{}]
    out = C.upkeep("verdict", repo, as_app=False, input=json.dumps({
        "head": pr["headRefOid"], "reviews": pr["reviews"]["nodes"], "comments": pr["comments"]["nodes"],
        "edited": pr.get("lastEditedAt"), "retargeted": retargets[-1].get("createdAt")}))
    if out["said"] is None:
        raise RuntimeError(out["why"])
    s = out["said"]
    st["since"] = s["since"]
    return s["verdict"], s["at"], s["rounds"]


def ask(repo, pr, st, why):
    """Ask the connector to look at the PR's head again, once a head and at
    most once an hour: after its findings were answered without a push, or
    when a push, a retarget or an edit brought no review. `why` goes in the comment. st is the PR's
    state. What GitHub said if it refused the comment, which isn't counted
    as asked (the caller strikes it), else None."""
    head = pr["headRefOid"]
    asked = st.setdefault("asked", {})
    # Once more, though, when the PR was retargeted or edited since.
    if asked.get("head") == head and C.NOW - asked.get("at", 0) < 60 * MIN and asked["at"] > st.get("since", 0):
        return None
    if DRY:
        log(f"  would: ask Codex to review {repo}#{pr['number']} at {head[:7]}")
        return None
    if C.comment(repo, pr["number"], f"{ASK}\n\n{why}") is None:
        return "GitHub refused the comment"
    st["asked"] = {"head": head, "at": C.NOW}
    log(f"{repo}#{pr['number']}: asked Codex to review {head[:7]}: {why}")
    return None
