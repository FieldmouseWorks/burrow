"""A repo's open PRs as GitHub has them, and what the steward writes back
about one: how its CI stands, its `review-settled` status, its stage
label, the merge."""

import re
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
            ... on CheckRun { databaseId externalId name status conclusion checkSuite { id status app { slug } workflowRun { file { path } workflow { id name } } } }
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


def ci_state(pr, proofs=(), moved=None, repo=None):
    """green / pending / red / none (no checks at all) over every check on
    the head except ours, the names of the ones that aren't green, and
    whether CI took the head in its docs lane for the PR's diff (runs, and
    docs_lane, which moved is for). That's read once, for the whole decision:
    what the step waives and what it says it landed on are the one answer,
    whatever GitHub would say if asked again. A proof (a check a Claude job
    runs on a machine) that hasn't reported yet is pending: nothing posts it
    until its job runs, and none is owed on a head in the docs lane.
    A workflow GitHub couldn't start (a bad file, a pin missing an input)
    leaves no check run, only its suite: red, or a broken workflow would read
    as no CI. A status is its proof's however it's spelled, since GitHub
    ignores a context's case, and goes by the name the repo gives the proof,
    one name once. A run of burrow's shared CI that judged another diff than the
    PR's (runs) isn't the PR's CI, none of its check runs; until its workflow
    has run through for the PR's diff, CI is pending. The merge train's
    status is its word on the PR, not a check of it, except its failure: CI
    failed on the head merged alone onto the default branch, which is the
    head's to fix. Only the App's counts, as with ours. A cancelled check run
    goes by later work in the effective check set (effective_checks), and with nothing
    after it is pending, since a stopped run found nothing, unless it ran out
    of time (timed_out), which is red. A workflow's
    check run that passed while its run is still going is pending: a job
    that needs it has no check run until it starts, so every check on the
    head can read green a moment before one more begins."""
    nodes = pr["commits"]["nodes"]
    commit = nodes[0]["commit"] if nodes else {}
    contexts = (commit.get("statusCheckRollup") or {}).get("contexts") or {}
    suites = commit.get("checkSuites") or {}
    if any((x.get("pageInfo") or {}).get("hasNextPage") for x in (contexts, suites)):
        # Only the first 100 come back; one past them could be red.
        return "pending", ["over 100 checks, too many to read"], False
    stale, docs, unrun = runs(pr, repo)
    docs = docs and docs_lane(pr, moved)
    if docs:
        proofs = ()
    spelled = {p.lower(): p for p in proofs}
    red, pending, seen = [], [], len(unrun)
    pending += [f"{w} for {pr['baseRefName']}" for w in unrun]
    for s in suites.get("nodes", []):
        if s["conclusion"] == "STARTUP_FAILURE":
            seen += 1
            flow = ((s.get("workflowRun") or {}).get("workflow") or {}).get("name") or "a workflow"
            red.append(f"{flow} (failed to start)")
    for c in effective_checks(contexts.get("nodes", []), stale, docs_lane(pr, moved)):
        if c["__typename"] == "CheckRun":
            seen += 1
            name = c["name"]
            if c["status"] != "COMPLETED":
                pending.append(name)
            elif cancelled(c) and timed_out(repo, c):
                # GitHub stops a job that runs past its timeout-minutes as
                # cancelled: the head's tests hang, or take too long, which
                # is the head's to fix.
                red.append(f"{name} (timed out)")
            elif cancelled(c):
                # Nothing ran it through since: the run was stopped (a runner
                # shut down under it, a newer run's concurrency, or someone
                # stopped it), not failed, so there's nothing in the head for
                # a fix job to fix. Pending until it's run again, and a stall
                # once that's overdue.
                pending.append(f"{name} (cancelled, to be run again)")
            elif c["conclusion"] not in ("SUCCESS", "NEUTRAL", "SKIPPED"):
                red.append(name)
            elif workflow(c) is not None and suite(c).get("status") != "COMPLETED":
                # Its run isn't over: a job that waits on this one has yet to
                # start, as the gate's check job waits on its legs.
                pending.append(f"{workflow_name(c) or 'CI'} (still running)")
        else:
            name = c["context"]
            if name == CONTEXT:
                continue
            if name == T.CONTEXT:
                if c["state"] != "FAILURE" or not C.mine(c.get("creator")):
                    continue
                name = f"{name} ({c.get('description') or 'red alone'})"
            seen += 1
            name = spelled.get(name.lower(), name)
            if c["state"] in ("PENDING", "EXPECTED"):
                pending.append(name)
            elif c["state"] != "SUCCESS":
                red.append(name)
    named = {c.get("name") or spelled.get(c["context"].lower(), c["context"]) for c in contexts.get("nodes", [])}
    for name in proofs:
        if name not in named:
            seen += 1
            pending.append(name)
    if not seen:
        return "none", [], docs
    return ("red" if red else "pending" if pending else "green"), list(dict.fromkeys(red or pending)), docs


# The names burrow's shared CI (flake-check.yml) gives two of its check runs
# on a PR, after the caller's job's name: its lane job's, `lane into BASE`,
# the base it chose the PR's gate against, and its docs lane's (docs-paths),
# `docs only into BASE from FORK`, FORK the commit the PR left BASE at. Which
# paths a PR changes, and so which lane it takes, depends on both.
LANE, DOCS_LANE = "lane into ", "docs only into "
# Its whole gate's job, skipped in the docs lane and on a PR the train lands.
# With the gate split across legs (its `legs`), each leg's job, `leg K of N`,
# does a share of that work, and the job named check says what they found.
GATE, LEG = "check", re.compile("leg [1-8] of [2-8]")
# The files of a repo's call to it, and of the same call for a PR's new base.
FLAKE = (".github/workflows/flake.yml", ".github/workflows/flake-retarget.yml")


def suite(c):
    """A check run's suite, its run of a workflow."""
    return c.get("checkSuite") or {}


def workflow(c):
    """Which workflow a check run's suite ran: its id, but one for the shared
    CI's caller and its twin for a new base (FLAKE), whose runs speak for each
    other. By file, not by name: a repo's unrelated workflows can share a name."""
    run = suite(c).get("workflowRun") or {}
    return FLAKE[0] if (run.get("file") or {}).get("path") in FLAKE else (run.get("workflow") or {}).get("id")


def workflow_name(c):
    """A check run's workflow's name, to say what CI waits on."""
    return ((suite(c).get("workflowRun") or {}).get("workflow") or {}).get("name")


def judged(c):
    """What a check run of GitHub Actions' says the shared CI judged its
    head against: (base, None) for its lane, (base, fork) for its docs lane,
    None for any other check. A job GitHub skipped keeps its name unfilled,
    `${{ … }}` and all, and no branch's name has a space in it (an ASCII
    one: git allows any other, a Unicode space included)."""
    if c["__typename"] != "CheckRun" or (suite(c).get("app") or {}).get("slug") != "github-actions":
        return None
    job = c["name"].rpartition(" / ")[2]
    if job.startswith(LANE) and re.fullmatch("[^ ]+", job[len(LANE):]):
        return job[len(LANE):], None
    m = re.fullmatch(rf"{DOCS_LANE}([^ ]+) from ([0-9a-f]{{40}})", job)
    return (m[1], m[2]) if m else None


def cancelled(c):
    """Whether a check run finished by being cancelled."""
    return c.get("status") == "COMPLETED" and c.get("conclusion") == "CANCELLED"


# What GitHub Actions says, in an annotation on the job's check run, of a
# job it stopped at its timeout-minutes: "The job running on runner NAME has
# exceeded the maximum execution time of N minutes." It ends the check run
# as cancelled, as a runner shutting down, a newer run's concurrency or a
# person's cancel does, and those say something else.
TIMED_OUT = "has exceeded the maximum execution time"


def timed_out(repo, c):
    """Whether a cancelled check run was stopped for running past its
    job's timeout, as its annotations say. Asked only of one nothing ran
    through since (ci_state), so rarely. A check run with no id to ask
    by (or no repo) wasn't. GitHub not answering fails the PR's step, to be
    tried again, rather than guess either way."""
    run = c.get("databaseId")
    if not run or not repo:
        return False
    # Every page: a job can warn many times before it runs out of time.
    pages = C.gh("api", "--paginate", "--slurp", f"repos/{repo}/check-runs/{run}/annotations?per_page=100")
    return any(TIMED_OUT in (a.get("message") or "") for page in (pages if isinstance(pages, list) else [])
               for a in (page if isinstance(page, list) else []) if isinstance(a, dict))


def effective_checks(contexts, stale, docs):
    """One ordered replacement relation for failures and cancellations alike.
    A later check may stand for earlier work only in its own workflow and
    PR lane. Skips did nothing; a cancelled check must wait for completed
    work, so a known timeout remains red while its retry is running. Only
    larger check IDs can replace predecessors: a discarded old success
    cannot clear its replacement's cancellation.

    Distinct jobs and matrix entries may share a display name. A name is
    usable across suites only when unambiguous in both; within a suite an
    explicit integrator job reference must identify the same job.
    Missing IDs or ambiguous names keep all checks in the decision.
    """
    checks = [c for c in contexts if c.get("__typename") == "CheckRun"]

    def unique(c):
        peers = [x for x in checks if suite(x).get("id") == suite(c).get("id") and x["name"] == c["name"]]
        identities = {x.get("externalId") or x.get("databaseId") or id(x) for x in peers}
        return len(identities) == 1

    def replaces(c, x):
        mine, newer = c.get("databaseId"), x.get("databaseId")
        flow = workflow(c)
        if (mine is None or newer is None or newer <= mine or flow is None or workflow(x) != flow
                or not suite(c).get("id") or not suite(x).get("id")
                or suite(x).get("id") in stale or x.get("conclusion") == "SKIPPED"):
            return False
        if not unique(c) or not unique(x):
            return False
        if suite(c).get("id") == suite(x).get("id") and not (
                c.get("externalId") and c["externalId"] == x.get("externalId")):
            return False
        if cancelled(c) and (not ran(x) or suite(x).get("status") != "COMPLETED"):
            return False
        # A shared call is identified even when its lane was skipped before
        # GitHub filled in the name. A sibling call cannot vouch for it.
        shared = any(workflow(y) == flow and job_of(y)[0] == job_of(c)[0]
                     and job_of(y)[1].startswith(LANE) for y in checks)
        if shared and not any(suite(y).get("id") == suite(x).get("id")
                              and job_of(y)[0] == job_of(c)[0] and ran(y) and judged(y)
                              and suite(y).get("id") not in stale for y in checks):
            return False
        return same_work(c, x, shared, docs)

    return [c for c in contexts if c.get("__typename") != "CheckRun"
            or (suite(c).get("id") not in stale and not any(replaces(c, x) for x in checks))]


def job_of(c):
    """A check run's name split: the call it's of, and its job."""
    caller, _, job = c["name"].rpartition(" / ")
    return caller, job


def same_work(c, x, shared, docs):
    """The same job, or explicit gate coverage in one validated shared call.
    A docs verdict replaces only gate work, and the whole gate can cover a
    previous docs job or leg. One leg never stands for a different leg.
    """
    if c["name"] == x["name"]:
        return True
    (mine, a), (theirs, b) = job_of(c), job_of(x)
    if not shared or mine != theirs:
        return False
    if a.startswith(LANE) and b.startswith(LANE):
        return True
    if not ran(x):
        return False
    return (gate_job(a) and b == GATE) or (docs and gate_job(a) and b.startswith(DOCS_LANE))


def gate_job(job):
    """Whether a job's name is the shared CI's gate for a PR, in either lane,
    or a leg of it."""
    return job == GATE or bool(LEG.fullmatch(job)) or job.startswith(DOCS_LANE)


def ran(c):
    """Whether a check run did its work: finished, neither skipped nor cancelled."""
    return c.get("status") == "COMPLETED" and c.get("conclusion") not in ("SKIPPED", "CANCELLED")


def fork_of(repo, pr):
    """The commit the PR's head leaves its base at now (their merge base),
    None if GitHub didn't say or the base branch is gone. It moves when the
    base takes in some of the PR's commits, and so does what the PR changes.
    Asked of the base branch by its name, so GitHub answers for its tip as
    it is at the asking, after any merge since the PR list was read (another
    PR's earlier in the same pass included): nothing read before stands in
    for it, the PR's baseRefOid least of all, which GitHub can leave at an
    older commit of the branch. The name goes in as C.compare quotes it, so
    it's that branch's, whatever it holds."""
    if not repo:
        return None
    got = C.gh("api", C.compare(repo, pr["baseRefName"], pr["headRefOid"]) + "?per_page=1", "--jq",
               "{fork: .merge_base_commit.sha}", check=False)
    fork = got.get("fork") if isinstance(got, dict) else None
    return fork if re.fullmatch("[0-9a-f]{40}", fork or "") else None


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
    now = fork_of(repo, pr) if any(b == base and fork for _, (b, fork) in said) else None
    stale = {suite(c).get("id") for c, (b, fork) in said if b != base or fork is not None and fork != now} - {None}
    docs = any(fork is not None and suite(c).get("id") not in stale and c["status"] == "COMPLETED"
               and c["conclusion"] == "SUCCESS" for c, (_, fork) in said)
    ran = {workflow(c) for c, (_, fork) in said
           if fork is None and suite(c).get("id") not in stale and suite(c).get("status") == "COMPLETED"} - {None}
    unrun = sorted({workflow_name(c) or "CI" for c, _ in said if suite(c).get("id") in stale and workflow(c) not in ran})
    return stale, docs, unrun


def docs_lane(pr, moved):
    """Whether a docs lane CI ran on this head for the PR's diff (runs) can
    stand for it: every path it changes since it left its base is then
    documentation, nothing the repo's flake builds or tests reads, so the
    repo's proofs, which check what it builds, have nothing of its to check.
    It can when the PR changes nothing under .github/: the workflow that
    judged it, and the docs paths it read, are then the base branch's own,
    not ones the PR wrote. moved: where its renames and copies started
    (touches.moved_from), None if that couldn't be read."""
    if moved is None or pr["changedFiles"] > len(pr["files"]["nodes"]):
        return False
    return not any(p == ".github" or p.startswith(".github/") for p in [f["path"] for f in pr["files"]["nodes"]] + list(moved))


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
