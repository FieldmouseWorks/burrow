"""A repo's open PRs as GitHub has them, and what the steward writes back
about one: how its CI stands, its `review-settled` status, the merge."""

import re
import urllib.parse

import common as C
import train as T
from common import CONTEXT, DRY, log


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
    defaultBranchRef { name target { ... on Commit { oid statusCheckRollup { state } } } }
    pullRequests(states: OPEN, first: 30, after: $cursor, orderBy: {field: CREATED_AT, direction: ASC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        id number title url isDraft isCrossRepository headRefName headRefOid baseRefName
        createdAt updatedAt mergeStateStatus body baseRef { target { oid } }
        author { login __typename }
        autoMergeRequest { enabledAt }
        mergeQueueEntry { id }
        labels(first: 100) { pageInfo { hasNextPage } nodes { name } }
        files(first: 100) { nodes { path changeType } }
        changedFiles
        reviewThreads(first: 100) { pageInfo { hasNextPage } nodes { isResolved } }
        reviews(last: 100) { pageInfo { hasPreviousPage startCursor } nodes { %s } }
        comments(last: 100) { pageInfo { hasPreviousPage startCursor } nodes { %s } }
        reactions(last: 20) { nodes { content createdAt user { login } } }
        timelineItems(last: 10, itemTypes: [RENAMED_TITLE_EVENT]) { nodes { ... on RenamedTitleEvent { createdAt previousTitle } } }
        commits(last: 1) {
          nodes { commit { oid statusCheckRollup { contexts(first: 100) { pageInfo { hasNextPage } nodes {
            __typename
            ... on CheckRun { name status conclusion checkSuite { id status app { slug } workflowRun { workflow { id name } } } }
            ... on StatusContext { context state description creator { login __typename } }
          } } }
          checkSuites(first: 100) { pageInfo { hasNextPage } nodes { conclusion workflowRun { workflow { name } } } } } }
        }
      }
    }
  }
}
"""


# What each review and comment is read for, in PRS and the older pages.
OLDER = {"reviews": "author { login __typename } state body submittedAt commit { oid }",
         "comments": "databaseId body createdAt updatedAt author { login __typename }"}
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


def ci_state(pr, proofs=(), moved=None, repo=None):
    """green / pending / red / none (no checks at all) over every check on
    the head except ours, and the names of the ones that aren't green. A
    proof (a check a Claude job runs on a machine) that hasn't reported yet
    is pending: nothing posts it until its job runs, and none is owed on a
    head CI took in its docs lane (docs_lane, which moved and repo are for).
    A workflow GitHub couldn't start (a bad file, a pin missing an input)
    leaves no check run, only its suite: red, or a broken workflow would read
    as no CI. A run of burrow's shared CI that judged another diff than the
    PR's (runs) isn't the PR's CI, none of its check runs; until its workflow
    has run through for the PR's diff, CI is pending. The merge train's
    status is its word on the PR, not a check of it, except its failure: CI
    failed on the head merged alone onto the default branch, which is the
    head's to fix. Only the App's counts, as with ours."""
    nodes = pr["commits"]["nodes"]
    commit = nodes[0]["commit"] if nodes else {}
    contexts = (commit.get("statusCheckRollup") or {}).get("contexts") or {}
    suites = commit.get("checkSuites") or {}
    if any((x.get("pageInfo") or {}).get("hasNextPage") for x in (contexts, suites)):
        # Only the first 100 come back; one past them could be red.
        return "pending", ["over 100 checks, too many to read"]
    if docs_lane(pr, moved, repo):
        proofs = ()
    stale, _, unrun = runs(pr, repo)
    red, pending, seen = [], [], len(unrun)
    pending += [f"{w} for {pr['baseRefName']}" for w in unrun]
    for s in suites.get("nodes", []):
        if s["conclusion"] == "STARTUP_FAILURE":
            seen += 1
            workflow = ((s.get("workflowRun") or {}).get("workflow") or {}).get("name") or "a workflow"
            red.append(f"{workflow} (failed to start)")
    for c in contexts.get("nodes", []):
        if c["__typename"] == "CheckRun":
            if suite(c).get("id") in stale:
                continue
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
            if name == T.CONTEXT:
                if c["state"] != "FAILURE" or not C.mine(c.get("creator")):
                    continue
                name = f"{name} ({c.get('description') or 'red alone'})"
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


# The names burrow's shared CI (flake-check.yml) gives two of its check runs
# on a PR, after the caller's job's name: its lane job's, `lane into BASE`,
# the base it chose the PR's gate against, and its docs lane's (docs-paths),
# `docs only into BASE from FORK`, FORK the commit the PR left BASE at. Which
# paths a PR changes, and so which lane it takes, depends on both.
LANE, DOCS_LANE = "lane into ", "docs only into "


def suite(c):
    """A check run's suite, its run of a workflow."""
    return c.get("checkSuite") or {}


def workflow(c):
    """The workflow a check run's suite ran: its id and name."""
    return (suite(c).get("workflowRun") or {}).get("workflow") or {}


def judged(c):
    """What a check run of GitHub Actions' says the shared CI judged its
    head against: (base, None) for its lane, (base, fork) for its docs lane,
    None for any other check. A job GitHub skipped keeps its name unfilled,
    `${{ … }}` and all, and no branch's name has a space in it."""
    if c["__typename"] != "CheckRun" or (suite(c).get("app") or {}).get("slug") != "github-actions":
        return None
    job = c["name"].rpartition(" / ")[2]
    if job.startswith(LANE) and re.fullmatch(r"\S+", job[len(LANE):]):
        return job[len(LANE):], None
    m = re.fullmatch(rf"{DOCS_LANE}(\S+) from ([0-9a-f]{{40}})", job)
    return (m[1], m[2]) if m else None


FORKS = {}


def fork_of(repo, pr):
    """The commit the PR's head leaves its base at now (their merge base),
    None if GitHub didn't say or the base branch is gone. It moves when the
    base takes in some of the PR's commits, and so does what the PR changes.
    Against the branch's tip as this pass read it (baseRef), never the PR's
    baseRefOid, which GitHub can leave at an older commit of the branch."""
    key = (repo, ((pr.get("baseRef") or {}).get("target") or {}).get("oid"), pr["headRefOid"])
    if repo and key[1] and key not in FORKS:
        got = C.gh("api", f"repos/{repo}/compare/{key[1]}...{key[2]}?per_page=1", "--jq",
                   "{fork: .merge_base_commit.sha}", check=False)
        if isinstance(got, dict) and re.fullmatch("[0-9a-f]{40}", got.get("fork") or ""):
            FORKS[key] = got["fork"]
    return FORKS.get(key)


def runs(pr, repo):
    """The shared CI's runs on the head, by what each judged (judged): the
    suites that judged another diff than the PR's, as it stands, whether one
    took the docs lane for the PR's own and passed it, and the workflows,
    by name, that have run on the head only for another diff. Another diff:
    another base (the PR retargeted since, an old run run again, another
    PR's on the same head), or a docs lane from another commit than the one
    the PR leaves its base at now. A workflow has run for the PR's diff once
    a run of it chose its lane against the PR's base and has finished, every
    job of it: a run still going may yet skip the gate."""
    base, nodes = pr["baseRefName"], pr["commits"]["nodes"]
    contexts = ((nodes[0]["commit"].get("statusCheckRollup") or {}).get("contexts") or {}).get("nodes", []) if nodes else []
    said = [(c, j) for c in contexts if (j := judged(c))]
    stale = {suite(c).get("id") for c, (b, fork) in said
             if b != base or fork is not None and fork != fork_of(repo, pr)} - {None}
    docs = any(fork is not None and suite(c).get("id") not in stale and c["status"] == "COMPLETED"
               and c["conclusion"] == "SUCCESS" for c, (_, fork) in said)
    ran = {workflow(c).get("id") for c, (_, fork) in said
           if fork is None and suite(c).get("id") not in stale and suite(c).get("status") == "COMPLETED"} - {None}
    unrun = sorted({workflow(c).get("name") or "CI" for c, _ in said
                    if suite(c).get("id") in stale and workflow(c).get("id") not in ran})
    return stale, docs, unrun


def docs_lane(pr, moved, repo=None):
    """Whether CI ran this head in the docs lane for the PR's diff (runs):
    every path it changes since it left its base is documentation, nothing
    the repo's flake builds or tests reads, so the repo's proofs, which check
    what it builds, have nothing of its to check. GitHub Actions passed the
    docs lane's check run for that base and fork on the head, and the PR
    changes nothing under .github/: the workflow that judged it, and the docs
    paths it read, are then the base branch's own, not ones the PR wrote.
    moved: where its renames and copies started (touches.moved_from), None
    if that couldn't be read."""
    if moved is None or pr["changedFiles"] > len(pr["files"]["nodes"]):
        return False
    if any(p == ".github" or p.startswith(".github/") for p in [f["path"] for f in pr["files"]["nodes"]] + list(moved)):
        return False
    return runs(pr, repo)[1]


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
    # sha stale, and it waits for the next pass.
    try:
        C.write("api", "-X", "PUT", f"repos/{repo}/pulls/{pr['number']}/merge",
                "-f", f"merge_method={method}", "-f", f"sha={pr['headRefOid']}")
    except RuntimeError as e:
        return C.said(e)
    return None
