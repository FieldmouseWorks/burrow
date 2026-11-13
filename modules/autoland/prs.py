"""A repo's open PRs as GitHub has them, and what the steward writes back
about one: how its CI stands, its `review-settled` status, its stage
label, the merge."""

import json
import subprocess
import urllib.parse

import common as C
import reviewers as V
import train as T
from common import CFG, CONTEXT, DRY, log


# What GitHub says of a PR's merge (its mergeStateStatus, every value GitHub's
# API has) and the step that calls for once it's settled. A value this doesn't
# know is "refused": not merged, and counted like a merge GitHub turned down.
MERGE_STEPS = {
    "CLEAN": "merge", "HAS_HOOKS": "merge",
    "UNSTABLE": "merge",  # a check failed that its rules don't require
    "BLOCKED": "merge",  # its rules want what settling gives: the steward's status, its required checks
    "BEHIND": "update",  # its rules want it up to date with its base
    "DIRTY": "conflict",
    "UNKNOWN": "wait", "DRAFT": "wait",  # GitHub is still working it out; a draft is seen to first
}

PRS = """
query($owner: String!, $name: String!, $cursor: String) {
  repository(owner: $owner, name: $name) {
    squashMergeAllowed mergeCommitAllowed rebaseMergeAllowed
    defaultBranchRef { name target { ... on Commit { %s } } }
    pullRequests(states: OPEN, first: 30, after: $cursor, orderBy: {field: CREATED_AT, direction: ASC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        id number title url isDraft isCrossRepository headRefName headRefOid baseRefName
        createdAt updatedAt mergeStateStatus body
        author { login __typename }
        autoMergeRequest { enabledAt }
        mergeQueueEntry { id }
        labels(first: 100) { pageInfo { hasNextPage } nodes { name } }
        closingIssuesReferences(first: 5) { nodes { id labels(first: 30) { nodes { name } } } }
        files(first: 100) { nodes { path changeType } }
        changedFiles
        reviewThreads(first: 100) { pageInfo { hasNextPage } nodes { isResolved } }
        reviews(last: 100) { pageInfo { hasPreviousPage startCursor } nodes { %s } }
        comments(last: 100) { pageInfo { hasPreviousPage startCursor } nodes { %s } }
        reactions(last: 20) { nodes { content createdAt user { login } } }
        timelineItems(last: 50, itemTypes: [UNLABELED_EVENT, RENAMED_TITLE_EVENT]) { pageInfo { hasPreviousPage } nodes { ... on UnlabeledEvent { createdAt label { name } } ... on RenamedTitleEvent { createdAt previousTitle } } }
        commits(last: 1) {
          nodes { commit { oid statusCheckRollup { contexts(first: 100) { pageInfo { hasNextPage } nodes {
            __typename
            ... on CheckRun { databaseId externalId name status conclusion checkSuite { id status app { slug } workflowRun { databaseId file { path } workflow { id name } } } }
            ... on StatusContext { context state description creator { login __typename } }
          } } }
          checkSuites(first: 100) { pageInfo { hasNextPage } nodes { id status conclusion workflowRun { databaseId createdAt file { path } workflow { id name } } checkRuns(first: 1) { totalCount } } } } }
        }
      }
    }
  }
}
"""


# What each review and comment is read for, in PRS and the older pages.
OLDER = {"reviews": "author { login __typename } state body submittedAt commit { oid }",
         "comments": "databaseId body createdAt updatedAt author { login __typename }"}
# The default branch's tip, read with the PRs each pass: how its CI stands,
# and what repos.landed_alone asks of it (its first parent's CI, the PR it
# landed, how its check runs ended), so a red one landing caused is undone
# with no further call.
TIP = ("oid statusCheckRollup { state contexts(first: 100) { nodes { ... on CheckRun { conclusion } } } } "
       "parents(first: 1) { nodes { statusCheckRollup { state } } } "
       "associatedPullRequests(first: 5) { nodes { number state headRefName baseRefName body mergeCommit { oid } } }")
PRS = PRS % (TIP, OLDER["reviews"], OLDER["comments"])
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


def has_ci(pr, proofs=()):
    """Whether CI checks the head: a check run, or a status that's neither
    ours, the merge train's, nor one of proofs (the checks a Claude job runs
    on a machine, which ci_state counts as pending before they've run), or
    a workflow GitHub couldn't start. A fix job's push gets the whole gate
    there, or else only from the job."""
    nodes = pr["commits"]["nodes"]
    commit = nodes[0]["commit"] if nodes else {}
    contexts = ((commit.get("statusCheckRollup") or {}).get("contexts") or {}).get("nodes", [])
    suites = (commit.get("checkSuites") or {}).get("nodes", [])
    theirs = {p.lower() for p in proofs} | {CONTEXT.lower()}
    return any(s["conclusion"] == "STARTUP_FAILURE" for s in suites) or any(
        c["__typename"] == "CheckRun" or (c["context"].lower() not in theirs and not c["context"].startswith(T.CONTEXT))
        for c in contexts)


def ci_state(pr, proofs=(), moved=None, repo=None, recover=False):
    """The Rust CI owner's verdict, over the pass's GraphQL nodes.
    Recovery is requested only by the PR step; pure readers never rerun.
    The bridge supplies the App token, dry-run flag and typed retry errors.
    """
    got = C.upkeep("ci", repo or "unused/unused", as_app=bool(repo), input=json.dumps({
        "pr": pr, "proofs": list(proofs), "moved": list(moved) if moved is not None else None,
        "remote": bool(repo), "recover": recover,
    }))["ci"]
    return got["state"], got["names"], got["docs"]


def our_status(pr, context=CONTEXT):
    """The steward's own status on the head: its context (or the train's,
    which cn posts as its App), posted by its App. Anyone who can push can
    post the context; theirs isn't the steward's word, and the ruleset
    doesn't count it either."""
    nodes = pr["commits"]["nodes"]
    rollup = (nodes[0]["commit"].get("statusCheckRollup") or {}) if nodes else {}
    for c in (rollup.get("contexts") or {}).get("nodes", []):
        if c["__typename"] == "StatusContext" and c["context"] == context and C.mine(c.get("creator")):
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


def show_waiting(repo, pr, labels, stage):
    """The PR's label for stage (stages.py), and none of the other stages',
    put on with its status, from the same step: none at all for NONE. labels
    is the PR's as this pass has them, kept up to date as they change. Best
    effort: one GitHub didn't take is asked for again by the next pass."""
    names = {k: x["name"] for k, x in CFG["labels"]["waiting"].items()}
    on = [names[stage]] if stage and names[stage] not in labels else []
    off = [x for k, x in names.items() if k != stage and x in labels]
    if not on and not off:
        return
    args = [a for x in on for a in ("--add-label", x)] + [a for x in off for a in ("--remove-label", x)]
    if C.write("pr", "edit", str(pr["number"]), "-R", repo, *args, check=False) is None:
        if not DRY:
            log(f"{repo}#{pr['number']}: GitHub didn't take its stage label ({' '.join(args)})")
        return
    labels.difference_update(off)
    labels.update(on)


# The first comment of each of a PR's review threads, read only for eyes():
# nested in PRS, a comments connection under every PR's threads would bill
# each page of the PR list a hundred times over, since GitHub charges a
# connection by its limits, not by what comes back.
THREADS = """
query($id: ID!) {
  node(id: $id) { ... on PullRequest { reviewThreads(first: 100) { nodes { isResolved comments(first: 1) { nodes { databaseId } } } } } }
}
"""


def eyes(repo, pr):
    """👀 on each open review thread's first comment: a Claude job has its
    findings. Best effort, and GitHub keeps one per person however often
    it's asked."""
    try:
        threads = C.graphql(THREADS, id=pr["id"])["node"]["reviewThreads"]["nodes"]
    except (RuntimeError, KeyError, TypeError):  # GitHub didn't say: no marker this time
        return
    for t in threads:
        first = t["comments"]["nodes"]
        if not t["isResolved"] and first:
            C.write("api", "-X", "POST", f"repos/{repo}/pulls/comments/{first[0]['databaseId']}/reactions",
                    "-f", "content=eyes", check=False)


def merge_method(repo, base, repo_info):
    """The first of squash, merge and rebase that the repo's settings, every
    ruleset on base and its classic protection allow, or None when there's
    none or GitHub didn't say what the rules are. Rules narrow the settings
    (a legacy ruleset allowing merge commits alone, or a linear history), and
    GitHub refuses any other method from the App, which bypasses no rule."""
    branch = urllib.parse.quote(base, safe="")
    pages = rules(repo, base, [], (C.NO_RULES,), f"repos/{repo}/rules/branches/{branch}?per_page=100", "--paginate",
                  "--slurp")
    protection = rules(repo, base, {}, (C.NO_RULES, UNPROTECTED), f"repos/{repo}/branches/{branch}/protection")
    if not isinstance(pages, list) or not isinstance(protection, dict):
        return None
    allowed = {"squash", "merge", "rebase"}
    for rule in (r for page in pages for r in page):
        if rule.get("type") == "pull_request":
            allowed &= set((rule.get("parameters") or {}).get("allowed_merge_methods") or allowed)
        elif rule.get("type") == "required_linear_history":
            allowed.discard("merge")
    if (protection.get("required_linear_history") or {}).get("enabled"):
        allowed.discard("merge")
    return next((method for flag, method in (("squashMergeAllowed", "squash"), ("mergeCommitAllowed", "merge"),
                                             ("rebaseMergeAllowed", "rebase"))
                 if repo_info[flag] and method in allowed), None)


# What the classic protection endpoint says of a branch without any. Any
# other 404 is GitHub not saying: it answers one for what a token can't see.
UNPROTECTED = "Branch not protected (HTTP 404)"


def rules(repo, base, none, absent, *args):
    """What GitHub says of base's rules at the path in args, `none` when it
    says there are none (one of the answers in absent: a private repo on a
    free plan has no rules, and a branch without classic protection is
    UNPROTECTED), or None when GitHub didn't say."""
    try:
        return C.gh("api", *args)
    except RuntimeError as e:
        if any(x in str(e) for x in absent):
            return none
        log(f"{repo}: the rules on {base}: {e}")
        return None


def onto(repo, pr):
    """The base's tip as GitHub has it now, and whether merging the PR onto
    it would change nothing: its head merged into that tip, as GitHub merges
    it, has the tip's own tree. Then what it changes landed some other way
    (another PR made the same change, red main's fixes most often), and its
    merge would only put an empty commit on the base. Read from the
    steward's mirror, fetched just now, so the tip is the one GitHub would
    merge it onto. A merge that conflicts, or a PR that has moved past the
    head this pass read (GitHub refuses that head's merge), changes
    something. Raises when git can't say; (None, False) in a dry run."""
    if DRY:
        return None, False
    n, head, base = pr["number"], pr["headRefOid"], pr["baseRefName"]
    mirror = V.fetch(repo, f"+refs/heads/{base}:refs/base/{base}", pr=pr)
    tip = V.git("-C", mirror, "rev-parse", f"refs/base/{base}").decode().strip()
    if V.git("-C", mirror, "rev-parse", f"refs/pr/{n}").decode().strip() != head:
        return tip, False
    merged = subprocess.run(["git", "-C", mirror, "merge-tree", "--write-tree", "--no-messages", tip, head],
                            capture_output=True, text=True)
    # 1 is a merge that conflicts: it would change the base, once resolved.
    if merged.returncode == 1:
        return tip, False
    if merged.returncode:
        raise RuntimeError(f"git merge-tree: {merged.stderr.strip()[-300:]}")
    return tip, merged.stdout.split()[0] == V.git("-C", mirror, "rev-parse", f"{tip}^{{tree}}").decode().strip()


def land(repo, pr, repo_info):
    """Merge it now, at the head that was checked, or not at all: None once
    GitHub has merged it, else why it hasn't (GitHub's own words, when it
    refused the merge). Nothing is left for GitHub to merge later
    (auto-merge, a merge queue): that would merge whatever head the base's
    rules let through, which needn't be one the steward checked. So a merge
    queued before, by anyone, is taken off first."""
    if not C.unland(repo, pr) and not DRY:
        return "GitHub didn't take off the merge already queued on it"
    method = merge_method(repo, pr["baseRefName"], repo_info)
    if method is None:
        return (f"not asked: no merge method both the repo's settings and the rules on {pr['baseRefName']} allow, "
                "or GitHub didn't say what the rules are")
    # The merge endpoint merges or refuses; a push since the check makes the
    # sha stale, and it waits for the next pass. A squash or a merge commit
    # takes the PR's title as its subject, the title the house check holds to
    # the house style: left to GitHub, a one-commit PR's squash would take that
    # commit's subject instead, which nothing checked.
    title = [] if method == "rebase" else ["-f", f"commit_title={pr['title']} (#{pr['number']})"]
    if C.paused(repo):
        raise C.JobPaused(f"{repo}: repository work paused before merge")
    try:
        C.write("api", "-X", "PUT", f"repos/{repo}/pulls/{pr['number']}/merge",
                "-f", f"merge_method={method}", *title, "-f", f"sha={pr['headRefOid']}")
    except RuntimeError as e:
        return C.said(e)
    return None
