"""The steward's reviewer: Codex, through its GitHub connector (the ChatGPT
Codex Connector App, set up on the owner's ChatGPT account to review every
push). It reviews each head on its own and posts on the PR: findings as a
review with inline threads, and a summary comment naming the commit it last
reviewed and whether it found anything. The steward reads its verdict from
those (`verdict`, burrow-upkeep's, crates/burrow-upkeep/src/verdict.rs).

The steward never asks it for a review: the connector takes `@codex review`
only from a GitHub user with a Codex account, and to the steward's App it
answers "create a Codex account and connect to github" (burrow#1216). So a
push is the only thing that brings a review: a head it skipped is listed
for the owner (review_step.py), and findings answered without a push are
judged (step.py)."""

import json

import common as C
import prs as P

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
