"""Who puts a PR first for its expedited step (expedite.py) is who put
`first` on it, as burrow-upkeep's firsts reads GitHub's labelled event, by
account id: an author's or the steward's word expedites its events; the
workers App's (which Claude jobs act as) or another account's, an author's
login and all, leaves them to the repo's ordinary pass. The rest of the
expedited step, with every other part stood in for, is expedite.py's own
test (tests/autoland/expedite.py)."""

import os
from unittest.mock import patch

from harness import C, S, fresh_pr_state, info, mkpr, serve_firsts, tmp

first = S.CFG["labels"]["first"]
AUTHOR, STEWARD = S.CFG["inbox"]["authors"][0]["id"], S.CFG["inbox"]["postingAccountId"]
WORKERS, SPOOF = 99, 98  # the machinery's App; an author's login on another account
told = f"{tmp}/told-prs"
with open(told, "w") as f:
    f.write("pr 4\n")


def events(by):
    return [{"event": "labeled", "label": {"name": first}, "actor": {"login": "sam", "id": by}}]


def pr(k, labels=()):
    p = mkpr(number=k, labels=labels, branch=f"b{k}", sha=f"{k}" * 40)
    p["closingIssuesReferences"] = {"pageInfo": {"hasNextPage": False, "endCursor": None}, "nodes": []}
    return p


for by, expedited in ((AUTHOR, True), (STEWARD, True), (WORKERS, False), (SPOOF, False)):
    fresh_pr_state()
    serve_firsts({"repos/sam/r/issues/4/events?per_page=100": events(by)})
    order = []
    with patch.object(S.R, "discover", lambda: [{"repo": "sam/r", "default": "main"}]), \
            patch.object(S.R, "settle_repo", lambda r, known: False), \
            patch.object(S.R, "publish", lambda *a: None), \
            patch.object(S, "open_prs", lambda owner, name: (info, [pr(4, (first,)), pr(9)])), \
            patch.object(S.step, "handle", lambda repo, p, *a, first=False: order.append((p["number"], first))), \
            patch.dict(os.environ, {"BURROW_EVENTS_PRS": told}):
        S.run_pass(["sam/r"])
    want = [(4, True)] if expedited else [(4, False), (9, False)]
    assert order == want, (by, order)
    assert (C.load("firsts.json", {}) != {}) == expedited, (by, C.load("firsts.json", {}))

print("a first PR's events are expedited only on an author's word: passed")
