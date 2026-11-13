"""Which of a repo's open PRs go first (labels.first): ahead in its line for
proofSlots (proofs.py) and in the pass's order (steward.py). One does when
one of the inbox's authors put `first` on it, or on an issue it closes; this
is the one place a PR gets the label from its issue, put on here so the PR
carries what its issue does (proj-inbox reads the same label on the issue:
crates/burrow-proj/src/inbox/first.rs).

Who put a label on is read from its last LabeledEvent's actor, by account
id: the authors' (inbox.authors) or the steward's own (postingAccountId),
never any text. The machinery's App, which Claude jobs act as, isn't one,
so a job can't move its own work up the line. It changes order only."""

import common as C
from common import CFG, log

# An issue's or a PR's labelled events, newest page first: the steward's
# stage labels come and go on every PR, so the one sought can be pages back.
LABELLED = """
query($id: ID!, $cursor: String) {
  node(id: $id) { ... on Issue { %(f)s } ... on PullRequest { %(f)s } }
}
""" % {"f": "timelineItems(last: 100, before: $cursor, itemTypes: [LABELED_EVENT]) { pageInfo { hasPreviousPage startCursor } "
            "nodes { ... on LabeledEvent { label { name } actor { __typename login ... on User { databaseId } ... on Bot { databaseId } } } } }"}
PAGES = 10


def trusted():
    """The account ids whose `first` counts."""
    inbox = CFG.get("inbox") or {}
    ids = {a.get("id") for a in inbox.get("authors", ())}
    ids.add(inbox.get("postingAccountId"))
    return ids - {None, 0}


def names(item):
    return {x.get("name") for x in (item.get("labels") or {}).get("nodes", ())}


def by_author(node_id):
    """Whether the last time `first` went on this issue or PR (its node id),
    one of `trusted` put it there. GitHub not saying is a no: the PR waits
    its turn as it would without the label."""
    want, cursor = CFG["labels"]["first"], None
    for _ in range(PAGES):
        try:
            got = C.graphql(LABELLED, id=node_id, cursor=cursor)["node"]["timelineItems"]
        except Exception as e:  # one unreadable mark mustn't stop the pass
            log(f"  who labelled {node_id} {want} isn't known: {C.said(e)}")
            return False
        for event in reversed(got["nodes"]):
            if (event.get("label") or {}).get("name") == want:
                return (event.get("actor") or {}).get("databaseId") in trusted()
        if not got["pageInfo"]["hasPreviousPage"]:
            return False
        cursor = got["pageInfo"]["startCursor"]
    return False


def firsts(repo, prs):
    """The numbers of the PRs that go first. One closing an issue an author
    marked first gets the label too, unless it has it already. One whose
    labels can't be read isn't first, and fails in its own step: one bad PR
    mustn't stop the rest."""
    want = CFG["labels"]["first"]
    out = set()
    for pr in prs:
        try:
            labelled = want in names(pr)
            if labelled and by_author(pr["id"]):
                out.add(pr["number"])
                continue
            issues = (pr.get("closingIssuesReferences") or {}).get("nodes") or ()
            if not any(want in names(i) and by_author(i["id"]) for i in issues):
                continue
            out.add(pr["number"])
            if not labelled:
                log(f"{repo}#{pr['number']}: closes an issue marked {want}; labelled {want}")
                C.write("pr", "edit", str(pr["number"]), "-R", repo, "--add-label", want, check=False)
        except Exception as e:  # one bad PR mustn't stop the rest
            log(f"{repo}#{pr.get('number')}: whether it goes first isn't known: {C.said(e)}")
    return out
