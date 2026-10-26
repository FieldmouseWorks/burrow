"""autoland-steward: every PR in every repo the owner has, from ready to merged.

A full pass (burrow-events runs one when it starts, and hourly, for what no
event reports):

  1. Find the repos: every non-archived, non-fork repo of each owner in
     `owners`, new ones included. Bring each one's settings to the baseline
     (auto-merge, delete-branch-on-merge, squash, secret scanning, the
     `baseline` ruleset); a repo seen for the first time also gets a Claude
     job that brings its files to docs/repo-baseline.md (repos.py).
  2. For each open PR that isn't a draft, work out where it stands and do
     the next thing: queue a review of its head from each reviewer, Codex
     and DeepSeek (reviewers.py), post what they found, hand open findings
     or red CI to a Claude job,
     have a Claude job run the checks only a machine of the owner's runs
     (proofJobs) or judge a PR it won't land on its own word (tasks.py says
     what each job is told), post the `review-settled` commit status and
     merge it, or list it for the owner.
  3. Once a day per repo, delete branches whose tip is already in the
     default branch, and list the stale unmerged ones.
  4. Keep one issue up to date with everything waiting for the owner.

`autoland-steward kick OWNER/REPO` is a pass over that repo's PRs alone
(2, and 4), the one burrow-events starts within seconds of what changes it:
a webhook delivery, a Claude job's turn ending, a reviewer's answer coming
back, or a wait the last pass booked ending (common.wake).

Everything talks to GitHub through `gh`, with the user's own login, so the
reviewers' reviews and the Claude jobs' verdicts are posted under it too.
Settings are in the JSON file $AUTOLAND_CONFIG (../autoland.nix). State is
in config.stateDir: one JSON file per PR, one per repo.

`autoland-steward status` prints what it thinks of every open PR without
changing anything.

`autoland-steward review` works through the reviews the passes queued
(review_worker.py), as the reviewers' own user. A careful look takes an
agent many minutes, so it runs as its own service and no pass waits for it.
"""

import fcntl
import os
import sys

import common as C
import repos as R
import reviewers as V
from common import CFG, CONTEXT, DRY, MIN, STATE, load, log, repo_cfg, save, ts
from touches import dep_kind, gate_edit, moved_from, one_way, renovate_edited, renovate_merges_now
from tasks import DEP_SEEN, JUDGMENT, MAJOR_VERDICT, REVIEWED, fix_task, judge_task, proof_task


PRS = """
query($owner: String!, $name: String!, $cursor: String) {
  repository(owner: $owner, name: $name) {
    squashMergeAllowed mergeCommitAllowed rebaseMergeAllowed
    pullRequests(states: OPEN, first: 30, after: $cursor, orderBy: {field: CREATED_AT, direction: ASC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        id number title body url isDraft isCrossRepository headRefName headRefOid baseRefName
        createdAt updatedAt mergeStateStatus
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
          } } } } }
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


def acted_on(pr):
    """Whether the steward does anything with the PR beyond saying it's not
    its to do: not a draft, and from one of its authors."""
    return not pr["isDraft"] and not pr["isCrossRepository"] and (pr["author"] or {}).get("login") in CFG["authors"]


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
    for pr in filter(acted_on, prs):
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
    is pending: nothing posts it until its job runs."""
    nodes = pr["commits"]["nodes"]
    rollup = (nodes[0]["commit"].get("statusCheckRollup") or {}) if nodes else {}
    contexts = rollup.get("contexts") or {}
    if (contexts.get("pageInfo") or {}).get("hasNextPage"):
        # Only the first 100 come back; one past them could be red.
        return "pending", ["over 100 checks, too many to read"]
    red, pending, seen = [], [], 0
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


def unland(repo, pr):
    """Something new came up after it was settled: stop the merge. False if
    GitHub didn't take it."""
    return not pr["autoMergeRequest"] or C.write(
        "pr", "merge", str(pr["number"]), "-R", repo, "--disable-auto", check=False) is not None


def handle(repo, pr, repo_info, digest):
    rc = repo_cfg(repo)
    n, head = pr["number"], pr["headRefOid"]
    # The judge reads the diff against the base, so its verdict (and its
    # job) is for this head on this base: one retargeted is judged again.
    judged_at = f"{head} {pr['baseRefName']}"
    key = f"pr-{repo.replace('/', '_')}-{n}.json"
    st = load(key, {})
    if st.get("head") != head:
        st.update(head=head, seen=C.NOW, handed=None, landFails=0, rv={})
    st.setdefault("rv", {})
    st.setdefault("rvBase", {})
    labels = {x["name"] for x in pr["labels"]["nodes"]}
    age = C.NOW - st["seen"]
    tag = f"{repo}#{n}"
    line = f"[{repo}#{n}]({pr['url']}) {pr['title']}"

    def after(t, minutes):
        """A wait that ends `minutes` after t: this repo gets a pass then."""
        C.wake(repo, t + minutes * MIN + 1)

    # Open a day, it's listed for the owner (below): a pass then says so.
    after(ts(pr["createdAt"]), 24 * 60)

    def handoff_due(kind):
        """Whether findings or red CI have sat long enough for a Claude job
        to take them (it's booked a pass for when they will, if not)."""
        since = max(ts(pr["updatedAt"]), st["seen"])
        after(since, rc["handoffAfter"])
        return C.NOW - since > rc["handoffAfter"] * MIN

    def settle(state, desc, why=None):
        ok = post_status(repo, pr, state, desc)
        if why:
            digest.append((why, line, desc))
        # Not settled (any more): nothing may merge it.
        if state != "success" and (pr["autoMergeRequest"] or st.get("landed")):
            if unland(repo, pr):
                st["landed"] = None
            else:
                ok = False
        if not ok and not DRY:
            # GitHub didn't take the status or the stop: until it does, the
            # old ones stand and auto-merge can still merge it. Again in a minute.
            after(C.NOW, 1)

    def update_branch():
        """Bring a settled head up to date with a base whose rules want it
        (auto-merge won't). The merged-in head is reviewed like any other."""
        if pr.get("mergeStateStatus") != "BEHIND" or st.get("updated") == head:
            return False
        if C.write("api", "-X", "PUT", f"repos/{repo}/pulls/{n}/update-branch",
                   "-f", f"expected_head_sha={head}", check=False) is None:
            return False
        log(f"{tag}: behind its base, brought up to date")
        st["updated"] = head
        return True

    def started(kind, task, at=head):
        """A Claude job, once per head (at: once per head on its base, for
        the judge's; recorded once it has started; until then it's tried
        again every five minutes and listed for the owner)."""
        jid = C.job(repo, task)
        if jid:
            st[kind] = at
            st[kind + "Job"] = jid
            log(f"{tag}: a Claude job {jid} ({kind})")
        else:
            log(f"{tag}: couldn't start a Claude job (no checkout, or proj refused)")
            after(C.NOW, 5)
        return bool(jid)

    def lost(kind, at=head):
        """This head's job of the kind ended without the answer it was for
        (it crashed, timed out, or never posted it). Seen on two passes: the
        first may have read the PR just before its answer went up."""
        jid = st.get(kind + "Job")
        if st.get(kind) != at or not jid or C.job_state(jid) in (None, "queued", "running"):
            return False
        seen = st.get(kind + "Lost") == at
        st[kind + "Lost"] = at
        if not seen:
            after(C.NOW, 1)  # the second look
        return seen

    def again(kind, task, doing, answer, at=head):
        """A job that ended without its answer is started once more; one that
        does that twice is listed for the owner."""
        if st.get(kind + "Again") == at:
            settle("pending", f"Settled, but two Claude jobs ended without {answer}"[:140],
                   f"Two Claude jobs ended without {answer}")
        elif started(kind, task, at):
            st[kind + "Again"] = at
            st.pop(kind + "Lost", None)
            settle("pending", f"Settled; a Claude job is {doing} again (the last one ended without {answer})"[:140])
        else:
            settle("pending", f"Settled, but no Claude job could start {doing} again"[:140],
                   "Couldn't start a Claude job")

    def fixing():
        """The kind of fix job handed on this head that's still queued or
        running (or that proj can't say about), else None."""
        for k in ("handed", "redFixed", "failFixed"):
            jid = st.get(k + "Job")
            if st.get(k) == (judged_at if k == "failFixed" else head) and jid \
                    and C.job_state(jid) in (None, "queued", "running"):
                return k
        return None

    def hand_off(why, kind="handed", at=head):
        """A Claude job fixes it, one of each kind a head (at). One that ended
        with this still to fix (it crashed, or pushed nothing) is handed once
        more, and after a second it's listed for the owner. Never while
        another fix job is still working on this head: two on one branch
        push over each other."""
        retry = st.get(kind) == at
        if retry and not lost(kind, at):
            return
        if retry and st.get(kind + "Again") == at:
            digest.append(("Two Claude jobs ended without fixing it", line, why))
            return
        busy = fixing()
        if busy:
            log(f"{tag}: a Claude job ({busy}) is still working on this head; {kind} waits for it")
            return
        proofs = {c: rc["proofJobs"][c] for c in ci_names if c in rc["proofJobs"]} if ci == "red" else {}
        if not started(kind, fix_task(repo, pr, why, proofs), at):
            digest.append(("Couldn't start a Claude job to fix it", line, why))
        elif retry:
            st[kind + "Again"] = at
            st.pop(kind + "Lost", None)

    def merge():
        # Marked landed only once GitHub has queued or done the merge, so a
        # failed one is tried again next pass, and the owner hears after three.
        if land(repo, pr, repo_info):
            st["landed"] = head
            return
        st["landFails"] = st.get("landFails", 0) + 1
        log(f"{tag}: merge failed ({st['landFails']})")
        after(C.NOW, 1)
        if st["landFails"] >= 3:
            digest.append(("Couldn't merge", line, "gh pr merge keeps failing (journalctl -u autoland)"))

    def land_now(desc):
        settle("success", desc)
        if st.get("landed") != head:
            log(f"{tag}: settled, landing")
            merge()
        else:
            update_branch()

    def owner_decides():
        """Sent to the owner by hand, by a fix job or by a judge (the label):
        listed for them, and no judge job."""
        judged_owner = C.marker(pr, JUDGMENT, head, repo) == "owner"
        if judged_owner:
            # The judge's word, seen labelled on this head: once the label
            # is off, that's the owner's go-ahead. A label taken off before
            # the judge said anything isn't an answer to it.
            st["ownerSaw"] = judged_at
        if st.get("escalated"):
            # Labelled here at the round limit: still theirs, clean reviews
            # since or not, until the label comes off.
            settle("pending", f"Past the review-round limit: {CFG['owner']} decides (the {CFG['labels']['owner']} label)",
                   "Unsettled after the round limit")
        else:
            settle("pending", f"Settled; {CFG['owner']} decides (the {CFG['labels']['owner']} label)",
                   "Judged yours to decide" if judged_owner else f"Labelled {CFG['labels']['owner']}")

    def judged(why, seen=REVIEWED):
        """Settled, but not something the steward lands on its own word: one
        Claude job a head judges it, and its verdict on this head decides."""
        verdict = C.marker(pr, JUDGMENT, head, repo)
        if verdict == "land":
            land_now(f"Settled; a Claude job judged it fine to land ({why})")
        elif verdict == "owner" and st.get("ownerSaw") == judged_at:
            land_now(f"Settled; a Claude job sent it to {CFG['owner']}, who took the {CFG['labels']['owner']} label off ({why})")
        elif verdict == "owner":
            # Labelled here, never by the judge, so the steward knows it went
            # on: the owner has a label to take off, and taking it off is
            # their answer even before a pass has seen it on.
            settle("pending", f"Settled; a Claude job judged it {CFG['owner']}'s to decide ({why})", "Judged yours to decide")
            if C.write("pr", "edit", str(n), "-R", repo, "--add-label", CFG["labels"]["owner"], check=False) is not None:
                st["ownerSaw"] = judged_at
        elif verdict == "failed":
            # The judge's own build or tests failed on this head: a fix job
            # has it, and its push is judged again. Its own once a head: a
            # job that answered a finding here without pushing didn't fix this.
            settle("pending", "Settled, but its build or tests fail; a Claude job is fixing it")
            hand_off("the Claude job that judged it found its build or tests failing on this head (its comment on the PR says what)",
                     "failFixed", judged_at)
        elif update_branch():
            settle("pending", "Settled, but behind its base: brought up to date, reviewed again")
        elif lost("judged", judged_at):
            again("judged", judge_task(repo, pr, why, ci == "none", seen), "judging it", "a verdict", judged_at)
        elif st.get("judged") == judged_at or started("judged", judge_task(repo, pr, why, ci == "none", seen), judged_at):
            settle("pending", f"Settled; a Claude job is judging it ({why})")
        else:
            settle("pending", f"Settled, but no Claude job could judge it ({why})", "Couldn't start a Claude job to judge it")

    def prove():
        """Only checks that run on a machine, not in CI, are left: a Claude
        job runs them on this head and records their verdicts."""
        if lost("proved"):
            again("proved", proof_task(repo, pr, ci_names, rc["proofJobs"]), "running " + ", ".join(ci_names), "its verdict")
        elif st.get("proved") == head or started("proved", proof_task(repo, pr, ci_names, rc["proofJobs"])):
            settle("pending", f"Settled; a Claude job is running {', '.join(ci_names)}"[:140])
        else:
            settle("pending", f"Settled, but no Claude job could run {', '.join(ci_names)}"[:140],
                   "Couldn't start a Claude job to run its checks")

    if pr["isDraft"]:
        # Work in progress, whoever's it is: nobody reviews it yet, and nobody
        # is told.
        settle("pending", "Draft: work in progress, reviewed once it's marked ready")
        save(key, st)
        return
    if not acted_on(pr):
        settle("pending", f"Outside contribution: {CFG['owner']} reviews and merges it", "Outside contributions")
        save(key, st)
        return
    # A list longer than one page could hide a hold, an open thread, a
    # changes-requested review or a verdict: fail closed. (open_prs reads
    # older reviews and comments, so those two are cut only past its cap.)
    cut = [k for k, more in (("labels", pr["labels"]["pageInfo"]["hasNextPage"]),
                             ("review threads", pr["reviewThreads"]["pageInfo"]["hasNextPage"]),
                             ("reviews", pr["reviews"]["pageInfo"]["hasPreviousPage"]),
                             ("comments", pr["comments"]["pageInfo"]["hasPreviousPage"])) if more]
    if cut:
        settle("pending", f"Too many {' and '.join(cut)} to read: {CFG['owner']} merges it", "Too much to read")
        save(key, st)
        return
    if CFG["labels"]["hold"] in labels:
        # Held after it was settled: take back the status and the queued merge.
        log(f"{tag}: on hold")
        settle("pending", f"On hold (the {CFG['labels']['hold']} label)")
        save(key, st)
        return

    ci, ci_names = ci_state(pr, rc["proofJobs"])
    # No checks ran at all ("none"). Unless the repo is set not to need any
    # (docs only), that's not green: nothing has tested it, so a judge job
    # runs the build and tests itself.
    if ci == "none" and not rc["requireCi"]:
        ci = "green"
    open_threads = sum(1 for t in pr["reviewThreads"]["nodes"] if not t["isResolved"])
    latest = {}
    for r in pr["reviews"]["nodes"]:
        who = (r["author"] or {}).get("login")
        if who and r["state"] in ("APPROVED", "CHANGES_REQUESTED", "DISMISSED"):
            latest[who] = r["state"]
    changes = [w for w, s in latest.items() if s == "CHANGES_REQUESTED"]
    dep = dep_kind(pr)
    gate = one_way(pr, rc, moved_from(repo, pr, st))
    renovate = pr["headRefName"].startswith("renovate/")
    proof_only = ci == "pending" and bool(ci_names) and all(c in rc["proofJobs"] for c in ci_names)
    # A minor Renovate PR a Claude job fixed carries Claude's code, and Renovate
    # won't merge it; a bot's PR that changes a workflow beyond repinning its
    # actions changes the gate. Either goes through the review loop below like
    # a Claude PR. While a Renovate one's CI is red it's renovate-sweep's to
    # hand over, findings and all: a job from each on one branch would push
    # over each other. The sweep's push makes a new head, and the reviewers read it
    # again.
    # A judge asked for on this head and base (the repo had no CI then, say)
    # still decides once CI shows up: its owner or failed verdict isn't
    # dropped for a green check.
    judge_said = C.marker(pr, JUDGMENT, head, repo)
    asked = st.get("judged") == judged_at or judge_said is not None
    # Its owner or failed verdict holds it whatever CI is still doing; only
    # a land waits for CI.
    held = asked and judge_said in ("owner", "failed")
    edited = renovate_edited(repo, pr, st) if dep == "minor" and not gate else None
    fixed = dep == "minor" and not gate and (edited or gate_edit(repo, pr, st))
    swept = fixed and ci == "red" and renovate

    # Dependency bumps: CI is the review. A minor one merges once CI is green
    # (a Renovate one if Renovate would merge it now: renovate_merges_now);
    # renovate-sweep fixes the red ones (and settles their threads) and
    # gives each major a verdict. Anything else that's open on one, a review
    # finding or a red Dependabot update, goes to a Claude job like any PR's.
    # Sent to the owner (by hand, a fix job or a judge): theirs, whatever else
    # is open on it, and nothing more is asked or handed off until the label
    # comes off. One past the round limit is listed as that, below.
    if CFG["labels"]["owner"] in labels:
        owner_decides()
        save(key, st)
        return

    if dep and not fixed:
        major_verdict = C.marker(pr, MAJOR_VERDICT, head, base=pr["baseRefName"]) if dep == "major" and renovate else None
        threads = f"{open_threads} open review thread{'s' * (open_threads > 1)}" if open_threads else ""
        if changes:
            threads = ", ".join([threads] * bool(threads) + ["changes requested by " + ", ".join(changes)])
        if renovate and ci == "red":
            settle("pending", "Dependency update, CI red; renovate-sweep has it" + (f" ({threads})" if threads else ""))
        elif renovate and dep == "major" and threads and not major_verdict:
            # Until it gives this head a verdict, renovate-sweep's job has it,
            # threads, reviews and all: one job on its branch.
            settle("pending", f"Major update, {threads}; renovate-sweep has it"[:140])
        elif renovate and dep == "major" and major_verdict == "held":
            # Held for the owner (renovate-sweep tells them): no job pushes
            # to it, threads or not, and changes what they're deciding on.
            settle("pending", "Major update; renovate-sweep held it: see its comment")
        elif threads or ci == "red":
            what = threads or "CI is red (" + ", ".join(ci_names) + ")"
            kind = "handed" if threads else "redFixed"
            settle("pending", f"Dependency update; {what}"[:140])
            if handoff_due(kind):
                hand_off(what, kind)
        elif dep == "major" and renovate and major_verdict != "ok":
            # renovate-sweep's verdict comes first, and its job runs the proofs
            # until then: neither a judge nor a proof job of the steward's
            # works the branch beside it.
            settle("pending", "Major update; waiting for renovate-sweep's verdict on this head")
        elif held:
            judged("a Claude job was asked to judge this head", DEP_SEEN)
        elif proof_only:
            prove()
        elif ci == "pending":
            settle("pending", "Dependency update, CI pending")
        elif gate or ci == "none" or asked:
            judged(f"a dependency update touching {gate}" if gate else "the repo has no CI" if ci == "none"
                   else "a Claude job was asked to judge this head",
                   DEP_SEEN + ("; renovate-sweep cleared the major version for this head" if major_verdict == "ok" else ""))
        elif dep == "major" and not renovate:
            judged("a major dependency update", DEP_SEEN)
        elif dep == "major":
            land_now("Major update renovate-sweep cleared, CI green")
        else:
            settle("success", "Dependency update, CI green")
            # Merged now, not on Renovate's next run (a median of 37 minutes
            # after green), but a Renovate one only if Renovate would merge it
            # now too: its config says so in the PR, and only Renovate's
            # commits are on it (an unknown is left to Renovate).
            if (not renovate or (edited is False and renovate_merges_now(pr))) and st.get("landed") != head:
                merge()
        # Renovate's stuck ones are renovate-sweep's; a Dependabot one that
        # still hasn't merged after a day is the owner's to look at.
        if (not renovate and C.NOW - ts(pr["createdAt"]) > 86400
                and not any(x[1] == line for x in digest)):
            digest.append(("Open over a day", line, f"Dependency update, CI {ci}"))
        save(key, st)
        return

    # The reviewers (reviewers.py): every one this repo has, but one out of
    # service (its usage spent, say) isn't waited for meanwhile. Each review
    # is queued for the reviewers' own service; a pass posts what comes back.
    names = V.on(repo)
    # What the reviewers weren't shown: their clean reviews don't cover it,
    # so a judge, who sees it all, looks before it lands.
    unseen = V.unseen(key, head, pr["baseRefName"])
    away = {r: V.out(r) for r in names}
    live = [r for r in names if not away[r]]
    for r in names:
        # Once a pass, whichever PR finds it out, while the others carry on
        # without it (with none left, the PR says so below): logging it back
        # in, say, is the owner's to do.
        if live and away[r] and not any(x[:2] == ("A reviewer is out of service", V.NAMES[r]) for x in digest):
            digest.append(("A reviewer is out of service", V.NAMES[r], away[r]))
    said = {r: V.verdict(pr, head, r) for r in live}  # verdict, when, rounds with findings

    def consume(r):
        """What came back from the reviewer's service for this head, if
        anything: posted as its review, or counted as a failure."""
        rs = st["rv"].setdefault(r, {})
        name = V.item_name(key, r, head, pr["baseRefName"])
        back = load(f"{V.DONE}/{name}.json", None)
        if back and not DRY:
            if back["result"] == "review":
                try:
                    v = V.post(repo, pr, r, back["answer"])
                    _, _, rr = said[r]
                    said[r] = (v, C.NOW, rr + (v == "findings"))
                    rs.pop("fails", None)
                    rs.pop("failedAt", None)
                except ValueError as e:  # an answer that's no review
                    rs["fails"], rs["failedAt"] = rs.get("fails", 0) + 1, C.NOW
                    log(f"{tag}: {V.NAMES[r]}'s answer wasn't a review ({rs['fails']}): {e}")
                except RuntimeError as e:  # GitHub: the same answer goes again next pass, twice more
                    rs["fails"], rs["failedAt"] = rs.get("fails", 0) + 1, C.NOW
                    log(f"{tag}: couldn't post {V.NAMES[r]}'s review ({rs['fails']}): {e}")
                    if rs["fails"] < 3:
                        after(C.NOW, 1)
                        return
            elif back["result"] == "out":
                o = load(V.OUT, {})
                o[r] = {"why": back["why"], "until": back["until"]}
                save(V.OUT, o)
                log(f"{tag}: {V.NAMES[r]} is out of service until {back['until']:.0f}: {back['why']}")
            else:  # the agent, the network, its JSON: tried again next pass
                rs["fails"], rs["failedAt"] = rs.get("fails", 0) + 1, C.NOW
                log(f"{tag}: {V.NAMES[r]}'s review failed ({rs['fails']}): {back.get('error', '')[:200]}")
            V.drop(name)
            rs.pop("queued", None)
            rs.pop("gone", None)
        elif rs.get("queued") and not DRY and not os.path.exists(os.path.join(STATE, V.QUEUE, name + ".json")):
            # Out of the queue with nothing back. However long an agent
            # thinks, its review stays queued until it's answered, so this
            # is a lost one; seen on two passes, since the service may have
            # answered between this pass's two looks.
            if rs.get("gone"):
                rs.pop("queued")
                rs.pop("gone")
                rs["fails"], rs["failedAt"] = rs.get("fails", 0) + 1, C.NOW
                log(f"{tag}: the queued {V.NAMES[r]} review left the queue without an answer")
            else:
                rs["gone"] = True
                after(C.NOW, 1)  # the second look

    for r in live:
        consume(r)
    live = [r for r in live if not V.out(r)]
    for r in names:
        if V.out(r):
            # Back in service then: its look at this head is asked for.
            C.wake(repo, load(V.OUT, {})[r]["until"] + 1)
    rounds_of = {r: said[r][2] - st["rvBase"].get(r, 0) for r in live}
    if st.get("escalated") and CFG["labels"]["owner"] not in labels:
        # The owner took the label off: another full set of rounds.
        for r in live:
            st["rvBase"][r] = st["rvBase"].get(r, 0) + rounds_of[r]
            rounds_of[r] = 0
        st["escalated"] = False
    rounds = max(rounds_of.values(), default=0)
    clean = bool(live) and all(said[r][0] == "clean" for r in live)

    def ask(r):
        """Queue the reviewer on this head, unless it's already on its way.
        Its looks at one head are bounded by `rounds`: each one with findings
        counts as a round."""
        rs = st["rv"].setdefault(r, {})
        if rs.get("queued"):
            return
        if DRY:
            log(f"  would: ask {V.NAMES[r]} to review {tag} at {head[:7]}")
            return
        if rs.get("fails", 0) >= 3 and C.NOW - rs.get("failedAt", 0) > 3600:
            rs["fails"] = 0  # an outage passes: three more tries an hour on
        if rs.get("fails", 0) >= 3:
            after(rs.get("failedAt", 0), 60)
            return
        try:
            V.queue(repo, pr, key, r)
        except Exception as e:  # the snapshot: GitHub, git, the disk
            rs["fails"], rs["failedAt"] = rs.get("fails", 0) + 1, C.NOW
            log(f"{tag}: couldn't queue {V.NAMES[r]} on {head[:7]} ({rs['fails']}): {str(e)[:200]}")
            after(C.NOW, 5)
            return
        log(f"{tag}: queued a {V.NAMES[r]} review of {head[:7]}")
        rs["queued"] = C.NOW

    # 1. The reviewers.
    if rounds >= rc["rounds"] and not clean:
        # Escalated only once the label is on: a pass that sees no label takes
        # it as the owner's go-ahead and starts the rounds over.
        if not st.get("escalated") and (C.write("pr", "edit", str(n), "-R", repo, "--add-label",
                                                CFG["labels"]["owner"], check=False) is not None or DRY):
            C.comment(repo, n, f"{rounds} review rounds and there are still findings, so this stops here and waits for "
                      f"{CFG['owner']}. Taking the `{CFG['labels']['owner']}` label off gives it another {rc['rounds']}.")
            st["escalated"] = True
        settle("pending", f"Still unsettled after {rounds} review rounds: {CFG['owner']} decides", "Unsettled after the round limit")
        save(key, st)
        return

    quiet = age >= rc["settleDelay"] * MIN
    if not quiet and any(said[r][0] == "none" for r in live):
        after(st["seen"], rc["settleDelay"])  # the reviewers are asked then
    waiting = []
    if not live:
        why = "; ".join(f"{V.NAMES[r]}: {away[r] or V.out(r)}" for r in names) or "none is on"
        waiting.append(f"No reviewer can review it now ({why})")
        digest.append(("No reviewer can review it", line, why))
    for r in live:
        if said[r][0] == "none":
            if quiet:
                ask(r)
            waiting.append(f"{V.NAMES[r]} is reviewing this commit" if st["rv"].get(r, {}).get("queued")
                           else f"Waiting for {V.NAMES[r]}'s review of this commit")
    # A look that can't happen, a first one or a second: the owner hears either way.
    stuck = []
    for r in live:
        if st["rv"].get(r, {}).get("fails", 0) >= 3:
            x = f"failed {st['rv'][r]['fails']} times on {head[:7]}"
            stuck.append(f"{V.NAMES[r]} {x}")
            digest.append((f"{V.NAMES[r]} couldn't review it", line, x + " (journalctl -u autoland-review)"))

    what = []
    if open_threads:
        what.append(f"{open_threads} open review thread{'s' * (open_threads > 1)}")
    if changes:
        what.append("changes requested by " + ", ".join(changes))
    if waiting:
        settle("pending", "; ".join(waiting + stuck + what))
        # One reviewer's threads needn't wait for the other to finish.
        if what and not swept and handoff_due("handed"):
            hand_off(", ".join(what))
    elif not clean or open_threads or changes:
        if not open_threads and not changes:
            # Every finding answered without a new commit: the reviewers that
            # had findings look again.
            for r in live:
                if said[r][0] == "findings":
                    after(said[r][1], rc["settleDelay"])
                    if C.NOW - said[r][1] > rc["settleDelay"] * MIN:
                        ask(r)
            settle("pending", "Findings answered; " + ("; ".join(stuck) if stuck else f"review round {rounds + 1} next"))
        else:
            settle("pending", f"Round {rounds}: " + ", ".join(what))
            if not swept and handoff_due("handed"):
                hand_off(", ".join(what))
    # 2. The reviewers are clean on this head and nothing is open: CI, then land.
    elif ci == "red":
        settle("pending", "CI is red: " + ", ".join(ci_names)[:100])
        # Its own once a head, apart from a findings job's: one that answered
        # a finding here without pushing didn't fix a check that failed since.
        if not swept and handoff_due("redFixed"):
            hand_off("CI is red (" + ", ".join(ci_names) + ")", "redFixed")
    elif held:
        judged("a Claude job was asked to judge this head")
    elif proof_only:
        prove()
    elif ci == "pending":
        settle("pending", "Settled; waiting for CI: " + ", ".join(ci_names)[:90])
    elif gate or unseen or ci == "none" or asked:
        judged(f"it touches {gate}" if gate else
               f"it changes {', '.join(unseen[:5])}{' and more' * (len(unseen) > 5)}, which the reviewers aren't shown" if unseen
               else "the repo has no CI" if ci == "none" else "a Claude job was asked to judge this head")
    else:
        land_now(f"{' and '.join(V.NAMES[r] for r in live)} found nothing on this commit, every thread is resolved, CI is green")

    # Anything not merged after a day, reviewed clean or not (a check that never
    # reports keeps it pending), unless it's already listed for the owner.
    if C.NOW - ts(pr["createdAt"]) > 86400 and not any(x[1] == line for x in digest):
        digest.append(("Open over a day", line, our_status(pr)[1] or "unsettled"))
    save(key, st)


# ---------------------------------------------------------------- main

def coverage(repos, covered):
    """repos.json, for renovate-sweep, and when: every repo the steward has,
    with how its proofs run and whether this pass saw to its PRs."""
    done = {r["repo"] for r in covered}
    return {"at": C.NOW, "repos": {r["repo"]: {"proofJobs": repo_cfg(r["repo"])["proofJobs"], "covered": r["repo"] in done}
                                   for r in repos}}


def main():
    if sys.argv[1:] == ["review"]:
        import review_worker
        review_worker.work()
        return
    try:
        for d in ("", V.QUEUE, V.SNAP, V.DONE, V.GIT):
            os.makedirs(os.path.join(STATE, d), exist_ok=True)
    except OSError:
        if not DRY:
            raise
    # `kick REPO...`: just those repos' PRs, now (burrow-events, on what
    # changed on GitHub or here, or a wait that has ended). The full pass,
    # when burrow-events starts and hourly, does the rest: the settings, new
    # repos' baseline jobs and branches.
    kick = [k.lower() for k in sys.argv[2:]] if sys.argv[1:2] == ["kick"] else None
    if not DRY:
        # One pass at a time, full or kicked: two would act on the same PRs.
        lock = open(os.path.join(STATE, "pass.lock"), "w")
        fcntl.flock(lock, fcntl.LOCK_EX)
    repos = R.discover()
    if kick is not None:
        repos = [r for r in repos if r["repo"].lower() in kick]  # GitHub's names are case-blind
    # Repos whose baseline job has started. A repo not in it gets one, a few
    # per pass so the first pass on a machine doesn't start them all at once.
    known = set(load("known.json", []))
    # Ones whose job didn't start: tried again a day later, so they don't
    # take every pass's slots.
    failed = load("baseline-failed.json", {})
    started = 0
    # What waits for the owner, by repo: a kicked pass replaces its repos'.
    digest = {}
    read, live = set(), set()  # the repos whose PRs it read, and their heads' snapshots
    covered = []
    for r in repos:
        repo = r["repo"]
        digest[repo] = mine = []
        try:
            new = kick is None and R.settle_repo(r, known)
            if (new and CFG["baselineJobs"] and started < CFG["baselinePerPass"]
                    and C.NOW - failed.get(repo, 0) > 86400):
                jid = C.job(repo, CFG["baselineTask"].replace("REPO", repo), clone=True)
                started += 1
                if jid:
                    log(f"{repo}: baseline job {jid}")
                    known.add(repo)
                    failed.pop(repo, None)
                else:
                    log(f"{repo}: baseline job didn't start (no checkout, or proj refused); trying again tomorrow")
                    failed[repo] = C.NOW
            elif new and not CFG["baselineJobs"]:
                known.add(repo)
            owner, name = repo.split("/")
            info, prs = open_prs(owner, name)
            read.add(repo.replace("/", "_"))
            heads = set()
            seen_to = True
            for pr in prs:
                heads.add(pr["headRefName"])
                live.add(V.snap_name(f"pr-{repo.replace('/', '_')}-{pr['number']}.json", pr["headRefOid"], pr["baseRefName"]))
                try:
                    handle(repo, pr, info, mine)
                except Exception as e:  # one bad PR mustn't stop the rest
                    log(f"{repo}#{pr['number']}: {e}")
                    seen_to = False
            if seen_to:
                covered.append(r)
            else:
                C.wake(repo, C.NOW + 5 * MIN)
            if CFG["cleanBranches"]:
                # Stale ones the last sweep found are listed every pass.
                R.sweep_branches(r, heads, mine, sweep=kick is None)
        except Exception as e:
            log(f"{repo}: {e}")
            C.wake(repo, C.NOW + 5 * MIN)  # GitHub, often: another look soon
    # This pass's wake-ups and owner's lines replace the ones it had for the
    # repos it saw; a full pass's, all of them.
    seen = {r["repo"] for r in repos}
    wakes = {k: v for k, v in load("wake.json", {}).items() if v > C.NOW and k not in seen}
    save("wake.json", {**wakes, **C.WAKE})
    if kick is not None:
        digest = {**{k: v for k, v in load("digest-repos.json", {}).items() if k not in seen}, **digest}
    save("digest-repos.json", digest)
    if kick is None:
        # For renovate-sweep: it lists the ok majors of the repos this pass
        # didn't see to (one whose pass failed among them), and tells its
        # jobs how every repo's proofs run.
        save("repos.json", coverage(repos, covered))
        save("known.json", sorted(known))
        save("baseline-failed.json", failed)
    if not DRY:
        V.sweep(read, live)
    if kick is None:
        R.unhook_gone(repos + R.hook_projects(repos))
    # Last: GitHub failing it leaves the pass's state saved. A line more than
    # one repo has (a reviewer out of service) is listed once.
    R.publish(list(dict.fromkeys(tuple(x) for repo in sorted(digest) for x in digest[repo])))


if __name__ == "__main__":
    main()
