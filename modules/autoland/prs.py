"""A repo's open PRs as GitHub has them, and what the steward writes back
about one: how its CI stands, its `review-settled` status, the merge."""

import common as C
from common import CONTEXT, DRY


PRS = """
query($owner: String!, $name: String!, $cursor: String) {
  repository(owner: $owner, name: $name) {
    squashMergeAllowed mergeCommitAllowed rebaseMergeAllowed
    defaultBranchRef { name target { ... on Commit { oid statusCheckRollup { state } } } }
    pullRequests(states: OPEN, first: 30, after: $cursor, orderBy: {field: CREATED_AT, direction: ASC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        id number title url isDraft isCrossRepository headRefName headRefOid baseRefName
        createdAt updatedAt mergeStateStatus body
        author { login }
        autoMergeRequest { enabledAt }
        labels(first: 100) { pageInfo { hasNextPage } nodes { name } }
        files(first: 100) { nodes { path changeType } }
        changedFiles
        reviewThreads(first: 100) { pageInfo { hasNextPage } nodes { isResolved } }
        reviews(last: 100) { pageInfo { hasPreviousPage startCursor } nodes { %s } }
        comments(last: 100) { pageInfo { hasPreviousPage startCursor } nodes { %s } }
        reactions(last: 20) { nodes { content createdAt user { login } } }
        commits(last: 1) {
          nodes { commit { oid statusCheckRollup { contexts(first: 100) { pageInfo { hasNextPage } nodes {
            __typename
            ... on CheckRun { name status conclusion }
            ... on StatusContext { context state description }
          } } }
          checkSuites(first: 100) { pageInfo { hasNextPage } nodes { conclusion workflowRun { workflow { name } } } } } }
        }
      }
    }
  }
}
"""


# What each review and comment is read for, in PRS and the older pages.
OLDER = {"reviews": "author { login } state body submittedAt commit { oid }",
         "comments": "databaseId body createdAt updatedAt author { login }"}
PRS = PRS % (OLDER["reviews"], OLDER["comments"])
OLDER_PAGE = """
query($id: ID!, $cursor: String) {
  node(id: $id) { ... on PullRequest { %s(last: 100, before: $cursor) { pageInfo { hasPreviousPage startCursor } nodes { %s } } } }
}
"""


# Pages of older reviews, and of older comments, read for a PR: up to 1,000
# of each. Anyone can comment on a public repo's PRs, and every event reads
# them again, so a PR longer than that is the owner's (the cut in handle).
OLDER_PAGES = 9


def open_prs(owner, name):
    """The repo's merge settings and every open PR, all pages, with the
    reviews and comments of each it acts on, up to OLDER_PAGES older pages:
    a long PR's verdicts, markers and rounds can be older than the last 100.
    An outside contribution's older pages aren't read: it's the owner's
    anyway."""
    prs, cursor = [], None
    while True:
        info = C.graphql(PRS, owner=owner, name=name, cursor=cursor)["repository"]
        page = info.pop("pullRequests")
        prs += page["nodes"]
        if not page["pageInfo"]["hasNextPage"]:
            break
        cursor = page["pageInfo"]["endCursor"]
    for pr in filter(C.acted_on, prs):
        for field, nodes in OLDER.items():
            for _ in range(OLDER_PAGES):
                if not pr[field]["pageInfo"]["hasPreviousPage"]:
                    break
                got = C.graphql(OLDER_PAGE % (field, nodes), id=pr["id"], cursor=pr[field]["pageInfo"]["startCursor"])
                older = got["node"][field]
                pr[field] = {"pageInfo": older["pageInfo"], "nodes": older["nodes"] + pr[field]["nodes"]}
    return info, prs


def ci_state(pr, proofs=()):
    """green / pending / red / none (no checks at all) over every check on
    the head except ours, and the names of the ones that aren't green. A
    proof (a check a Claude job runs on a machine) that hasn't reported yet
    is pending: nothing posts it until its job runs. A workflow GitHub
    couldn't start (a bad file, a pin missing an input) leaves no check run,
    only its suite: red, or a broken workflow would read as no CI."""
    nodes = pr["commits"]["nodes"]
    commit = nodes[0]["commit"] if nodes else {}
    contexts = (commit.get("statusCheckRollup") or {}).get("contexts") or {}
    suites = commit.get("checkSuites") or {}
    if any((x.get("pageInfo") or {}).get("hasNextPage") for x in (contexts, suites)):
        # Only the first 100 come back; one past them could be red.
        return "pending", ["over 100 checks, too many to read"]
    red, pending, seen = [], [], 0
    for s in suites.get("nodes", []):
        if s["conclusion"] == "STARTUP_FAILURE":
            seen += 1
            workflow = ((s.get("workflowRun") or {}).get("workflow") or {}).get("name") or "a workflow"
            red.append(f"{workflow} (failed to start)")
    for c in contexts.get("nodes", []):
        if c["__typename"] == "CheckRun":
            seen += 1
            name = c["name"]
            if c["status"] != "COMPLETED":
                pending.append(name)
            elif c["conclusion"] not in ("SUCCESS", "NEUTRAL", "SKIPPED"):
                red.append(name)
        else:
            name = c["context"]
            if name == CONTEXT:
                continue
            seen += 1
            if c["state"] in ("PENDING", "EXPECTED"):
                pending.append(name)
            elif c["state"] != "SUCCESS":
                red.append(name)
    named = {c.get("name") or c.get("context") for c in contexts.get("nodes", [])}
    for name in proofs:
        if name not in named:
            seen += 1
            pending.append(name)
    if not seen:
        return "none", []
    return ("red" if red else "pending" if pending else "green"), red or pending


def our_status(pr):
    nodes = pr["commits"]["nodes"]
    rollup = (nodes[0]["commit"].get("statusCheckRollup") or {}) if nodes else {}
    for c in (rollup.get("contexts") or {}).get("nodes", []):
        if c["__typename"] == "StatusContext" and c["context"] == CONTEXT:
            return c["state"].lower(), c.get("description") or ""
    return None, ""


def post_status(repo, pr, state, desc):
    """False if GitHub didn't take it."""
    desc = desc[:140]
    if our_status(pr) == (state, desc):
        return True
    return C.write("api", f"repos/{repo}/statuses/{pr['headRefOid']}", "-f", f"state={state}",
                   "-f", f"context={CONTEXT}", "-f", f"description={desc}", "-f", f"target_url={pr['url']}",
                   check=False) is not None


def merge_method(repo_info):
    for flag, method in (("squashMergeAllowed", "--squash"), ("mergeCommitAllowed", "--merge"),
                         ("rebaseMergeAllowed", "--rebase")):
        if repo_info[flag]:
            return method
    return "--merge"


def land(repo, pr, repo_info):
    """Auto-merge (or merge now if nothing is left to wait for). False if
    GitHub took neither."""
    n = str(pr["number"])
    if not pr["autoMergeRequest"]:
        # Only the head that was checked: a push since then waits for the next pass.
        method = [merge_method(repo_info), "--match-head-commit", pr["headRefOid"]]
        if C.write("pr", "merge", n, "-R", repo, "--auto", *method, check=False) is None and not DRY:
            # No branch rules to wait on: GitHub won't queue it, so merge it now.
            return C.write("pr", "merge", n, "-R", repo, *method, check=False) is not None
    return True
