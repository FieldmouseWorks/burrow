"""autoland-steward: every PR in every repo the owner has, from ready to merged.

One pass (the timer runs it every couple of minutes):

  1. Find the repos: every non-archived, non-fork repo of each owner in
     `owners`, new ones included. Bring each one's settings to the baseline
     (auto-merge, delete-branch-on-merge, squash, secret scanning, the
     `baseline` ruleset); a repo seen for the first time also gets a Claude
     job that brings its files to docs/repo-baseline.md (repos.py).
  2. For each open PR that isn't a draft, work out where it stands and do
     the next thing: ask Codex for a review (and queue one from DeepSeek, if
     it's on: deepseek.py), hand open findings or red CI to a Claude job,
     have a Claude job run the checks only a machine of the owner's runs
     (proofJobs) or judge a PR it won't land on its own word, post the
     `review-settled` commit status and merge it, or list it for the owner.
  3. Once a day per repo, delete branches whose tip is already in the
     default branch, and list the stale unmerged ones.
  4. Keep one issue up to date with everything waiting for the owner.

Everything talks to GitHub through `gh`, with the user's own login, so
DeepSeek's reviews and the Claude jobs' verdicts are posted under it too.
Settings are in the JSON file $AUTOLAND_CONFIG (../autoland.nix). State is
in config.stateDir: one JSON file per PR, one per repo.

`autoland-steward status` prints what it thinks of every open PR without
changing anything.

`autoland-steward deepseek` works through the DeepSeek reviews the passes
queued (config.stateDir/ds-queue), a few at once, and posts each one. A
careful look takes DeepSeek many minutes, so it runs as its own service and
no pass waits for it.
"""

import json
import os
import re
import sys

import common as C
import deepseek as D
import repos as R
from common import CFG, CODEX, CONTEXT, DEP_RE, DRY, MIN, STATE, load, log, repo_cfg, save, ts
from tasks import DEP_SEEN, JUDGMENT, MAJOR_VERDICT, REVIEWED, fix_task, judge_task, proof_task


PRS = """
query($owner: String!, $name: String!, $cursor: String) {
  repository(owner: $owner, name: $name) {
    squashMergeAllowed mergeCommitAllowed rebaseMergeAllowed
    pullRequests(states: OPEN, first: 30, after: $cursor, orderBy: {field: CREATED_AT, direction: ASC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        id number title url isDraft isCrossRepository headRefName headRefOid baseRefName
        createdAt updatedAt mergeStateStatus
        author { login }
        autoMergeRequest { enabledAt }
        labels(first: 100) { pageInfo { hasNextPage } nodes { name } }
        files(first: 100) { nodes { path changeType } }
        changedFiles
        reviewThreads(first: 100) { pageInfo { hasNextPage } nodes { isResolved } }
        reviews(last: 100) { pageInfo { hasPreviousPage } nodes { author { login } state body submittedAt commit { oid } } }
        comments(last: 100) { pageInfo { hasPreviousPage } nodes { databaseId body createdAt updatedAt author { login } } }
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


def open_prs(owner, name):
    """The repo's merge settings and every open PR, all pages."""
    prs, cursor = [], None
    while True:
        info = C.graphql(PRS, owner=owner, name=name, cursor=cursor)["repository"]
        page = info.pop("pullRequests")
        prs += page["nodes"]
        if not page["pageInfo"]["hasNextPage"]:
            return info, prs
        cursor = page["pageInfo"]["endCursor"]


STATE_RE = re.compile(r"codex-review-state:v1 (\{.*?\}) -->", re.S)
RECEIPT_RE = re.compile(r"Reviewed commit:\*\*\s*`([0-9a-f]{7,40})`")


def codex_verdict(pr, head, since):
    """What Codex last said about this exact head: clean, findings, running,
    failed, or none. `since` is when the head first showed up."""
    events = []  # (time, verdict)
    for c in pr["comments"]["nodes"]:
        if (c["author"] or {}).get("login") != CODEX:
            continue
        body = c["body"]
        m = STATE_RE.search(body)
        if m:
            try:
                cr = json.loads(m.group(1))["code_review"]
            except (ValueError, KeyError):
                cr = {}
            if cr.get("commit_sha") == head:
                v = {"completed": "findings" if cr.get("has_any_findings") else "clean",
                     "running": "running"}.get(cr.get("status"), "none")
                events.append((ts(c["updatedAt"]), v))
            continue
        # A receipt names only a prefix of the commit: enough to count its
        # findings, never to call a head clean (the summary above does that).
        m = RECEIPT_RE.search(body)
        if m and head.startswith(m.group(1)) and "find any major issues" not in body:
            events.append((ts(c["createdAt"]), "findings"))
        elif "Something went wrong" in body and ts(c["createdAt"]) >= since:
            events.append((ts(c["createdAt"]), "failed"))
    for r in pr["reviews"]["nodes"]:
        if (r["author"] or {}).get("login") == CODEX and (r["commit"] or {}).get("oid") == head:
            clean = "find any major issues" in (r["body"] or "")
            events.append((ts(r["submittedAt"]), "clean" if clean else "findings"))
    # Eyes (working on it) on the PR. A thumbs-up names no commit (it can be
    # the end of a review of the previous head), so only the SHA-bound
    # summary and receipts above can say clean. (Eyes on a request comment
    # aren't read: the summary says "running" too, and reading every
    # comment's reactions made each pass's query cost ~60 points a repo.)
    for holder in [pr]:
        for x in (holder.get("reactions") or {}).get("nodes", []):
            if (x["user"] or {}).get("login") == CODEX and ts(x["createdAt"]) >= since and x["content"] == "EYES":
                events.append((ts(x["createdAt"]), "running"))
    if not events:
        return "none", 0
    t, v = max(events)
    return v, t


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
    desc = desc[:140]
    if our_status(pr) == (state, desc):
        return
    C.write("api", f"repos/{repo}/statuses/{pr['headRefOid']}", "-f", f"state={state}",
            "-f", f"context={CONTEXT}", "-f", f"description={desc}", "-f", f"target_url={pr['url']}", check=False)


def one_way(pr, rc, moved=()):
    """The first path that makes this PR one a Claude job judges before it
    lands (config oneWay), or None. moved: where its renames and copies
    started (moved_from), None if that couldn't be read."""
    for path in [f["path"] for f in pr["files"]["nodes"]] + list(moved or ()):
        for pat in rc["oneWay"]:
            if re.search(pat, path):
                return path
    if pr["changedFiles"] > len(pr["files"]["nodes"]):
        return f"{pr['changedFiles']} files (too many to check)"
    if moved is None:
        return "a rename or copy whose diff couldn't be read"
    return None


MOVED_RE = re.compile(r"^(?:rename|copy) from (.+)$")


def unquote(path):
    """A path as git's diff header has it: quoted, with C escapes, when it
    has odd characters."""
    if not (len(path) > 1 and path[0] == path[-1] == '"'):
        return path
    try:
        return path[1:-1].encode("ascii").decode("unicode_escape").encode("latin-1").decode("utf-8")
    except UnicodeError:
        return path


def moved_from(repo, pr, st):
    """Where the PR's renames and copies started: the file list names only
    where each one went, so a rename out of a one-way path (CODEOWNERS to
    CODEOWNERS.bak) is read from the diff. Read once per head and base; None
    if the diff couldn't be read."""
    if not any(f.get("changeType") in ("RENAMED", "COPIED") for f in pr["files"]["nodes"]):
        return []
    seen = [pr["headRefOid"], pr["baseRefName"]]
    if st.get("moved", [None])[:2] != seen:
        diff = C.gh("pr", "diff", str(pr["number"]), "-R", repo, check=False)
        if not isinstance(diff, str) or not diff:
            return None
        st["moved"] = seen + [[unquote(m.group(1)) for m in map(MOVED_RE.match, diff.splitlines()) if m]]
    return st["moved"][2]


# "from 1.2.3 to 2.0.0", "from v3 to v4" (an action's major tag), "from 3 to 4".
MAJOR_RE = re.compile(r"from v?(\d+)\S* to v?(\d+)")


def dep_kind(pr):
    """None, 'minor' or 'major' for a dependency bot's PR."""
    if not DEP_RE.match(pr["headRefName"]):
        return None
    labels = {x["name"] for x in pr["labels"]["nodes"]}
    if "security" in labels and pr["headRefName"].startswith("renovate/"):
        # Renovate's security fixes merge themselves, majors too: none waits
        # for renovate-sweep's verdict (renovate-sweep.sh).
        return "minor"
    m = MAJOR_RE.search(pr["title"])
    if "major" in labels or (m and m.group(1) != m.group(2)):
        return "major"
    return "minor"


def renovate_edited(repo, pr, st):
    """Whether a Renovate branch has a commit Renovate didn't write (a Claude
    job's fix). Renovate won't merge a branch edited like that. Asked once
    per head; a failed ask counts as not edited and is asked again."""
    if not CFG["renovateEmail"] or not pr["headRefName"].startswith("renovate/"):
        return False
    if st.get("editedHead") != pr["headRefOid"]:
        got = C.gh("pr", "view", str(pr["number"]), "-R", repo, "--json", "commits", check=False)
        if not got:
            return False
        st["edited"] = any(a.get("email") != CFG["renovateEmail"]
                           for c in got["commits"] for a in c["authors"])
        st["editedHead"] = pr["headRefOid"]
    return st["edited"]


WORKFLOW_RE = re.compile(r"^\.github/workflows/[^/]+\.ya?ml$")
# A line that only names an action at a ref: "uses: owner/action@sha # v4.1.0".
# The ref is a sha, tag or branch and nothing else, so no shell can ride on it.
PIN_LINE_RE = re.compile(r"^(\s*(-\s+)?uses:\s*[^\s#@]+@)[0-9A-Za-z._/-]+(\s+#.*)?$")


def pins_only(repo, pr):
    """True if every file the PR's diff touches under .github/ is a workflow
    whose changes only repin actions: each removed `uses:` line is replaced in
    place by one that's the same up to its `@` (indentation, key and action),
    so it's the same step at a new ref, and nothing is added, dropped or moved
    (no permissions, triggers or commands, and no `uses:`-shaped text inside
    a `run:`). A rename or copy into or out of .github/, or a file with no
    repinned line (a mode change, a binary), is not a repin. None if the diff
    couldn't be read."""
    diff = C.gh("pr", "diff", str(pr["number"]), "-R", repo, check=False)
    if not isinstance(diff, str) or not diff:
        return None
    path, hunk, gated, pinned, out, back = None, False, set(), set(), [], []

    def paired():
        # A run of removed lines, then the run that replaced them, line by line.
        ok = len(out) == len(back) and all(a.group(1) == b.group(1) for a, b in zip(out, back))
        if out and ok:
            pinned.add(path)
        out.clear()
        back.clear()
        return ok

    for x in diff.splitlines():
        if not (hunk and x[:1] in "+-\\") or (x[:1] == "-" and back):
            if not paired():
                return False
        if x.startswith("diff --git "):
            a, sep, b = x[len("diff --git "):].partition(" b/")
            if not (sep and a.startswith("a/")):
                if ".github/" in x:
                    return False
                path = None
                continue
            a, path, hunk = a[2:], b, False
            if a.startswith(".github/") or b.startswith(".github/"):
                if a != b or not WORKFLOW_RE.match(b):
                    return False
                gated.add(b)
            else:
                path = None
        elif x.startswith("@@"):
            hunk = True
        elif path and hunk and x[:1] in "+-" and x[1:].strip():
            m = PIN_LINE_RE.match(x[1:])
            if not m:
                return False
            (out if x[0] == "-" else back).append(m)
    return paired() and gated <= pinned


def gate_edit(repo, pr, st):
    """Whether a dependency bot's PR changes .github/ beyond repinning the
    actions its workflows use: that's the gate itself, and a dependency
    update is never reviewed. The file list names only a rename's new path,
    so a rename or copy is read from the diff too. (A list too long to see
    whole is already the owner's: one_way.) Read once per head and base; a diff that can't be read counts
    as an edit (reviewed, not merged on CI alone)."""
    nodes = pr["files"]["nodes"]
    if not any(f["path"].startswith(".github/") or f["changeType"] in ("RENAMED", "COPIED") for f in nodes):
        return False
    if any(f["path"].startswith(".github/") and not WORKFLOW_RE.match(f["path"]) for f in nodes):
        return True
    seen = [pr["headRefOid"], pr["baseRefName"]]
    if st.get("pins", [None])[:2] != seen:
        pins = pins_only(repo, pr)
        if pins is None:
            return True
        st["pins"] = seen + [pins]
    return not st["pins"][2]


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
    """Something new came up after it was settled: stop the merge."""
    if pr["autoMergeRequest"]:
        C.write("pr", "merge", str(pr["number"]), "-R", repo, "--disable-auto", check=False)


def handle(repo, pr, repo_info, digest):
    rc = repo_cfg(repo)
    n, head = pr["number"], pr["headRefOid"]
    # The judge reads the diff against the base, so its verdict (and its
    # job) is for this head on this base: one retargeted is judged again.
    judged_at = f"{head} {pr['baseRefName']}"
    key = f"pr-{repo.replace('/', '_')}-{n}.json"
    st = load(key, {})
    if st.get("head") != head:
        st.update(head=head, seen=C.NOW, asked=[], handed=None, landFails=0, ds={})
    st.setdefault("base", 0)
    st.setdefault("dsBase", 0)
    st.setdefault("ds", {})
    labels = {x["name"] for x in pr["labels"]["nodes"]}
    author = (pr["author"] or {}).get("login", "")
    age = C.NOW - st["seen"]
    tag = f"{repo}#{n}"
    line = f"[{repo}#{n}]({pr['url']}) {pr['title']}"

    def settle(state, desc, why=None):
        post_status(repo, pr, state, desc)
        if why:
            digest.append((why, line, desc))
        # Not settled (any more): nothing may merge it.
        if state != "success" and (pr["autoMergeRequest"] or st.get("landed")):
            unland(repo, pr)
            st["landed"] = None

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
        every pass and listed for the owner)."""
        jid = C.job(repo, task)
        if jid:
            st[kind] = at
            st[kind + "Job"] = jid
            log(f"{tag}: a Claude job {jid} ({kind})")
        else:
            log(f"{tag}: couldn't start a Claude job (no checkout, or proj refused)")
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
    # A list longer than one page could hide a hold, an open thread, a
    # changes-requested review or a verdict: fail closed.
    cut = [k for k, more in (("labels", pr["labels"]["pageInfo"]["hasNextPage"]),
                             ("review threads", pr["reviewThreads"]["pageInfo"]["hasNextPage"]),
                             ("reviews", pr["reviews"]["pageInfo"]["hasPreviousPage"]),
                             ("comments", pr["comments"]["pageInfo"]["hasPreviousPage"])) if more]
    if cut:
        settle("pending", f"Over 100 {' and '.join(cut)}, too many to read: {CFG['owner']} merges it", "Too much to read")
        save(key, st)
        return
    if CFG["labels"]["hold"] in labels:
        # Held after it was settled: take back the status and the queued merge.
        log(f"{tag}: on hold")
        settle("pending", f"On hold (the {CFG['labels']['hold']} label)")
        save(key, st)
        return
    if pr["isCrossRepository"] or author not in CFG["authors"]:
        settle("pending", f"Outside contribution: {CFG['owner']} reviews and merges it", "Outside contributions")
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
        if who and who != CODEX and r["state"] in ("APPROVED", "CHANGES_REQUESTED", "DISMISSED"):
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
    # over each other. The sweep's push makes a new head, and Codex reads it
    # again.
    # A judge asked for on this head and base (the repo had no CI then, say)
    # still decides once CI shows up: its owner or failed verdict isn't
    # dropped for a green check.
    judge_said = C.marker(pr, JUDGMENT, head, repo)
    asked = st.get("judged") == judged_at or judge_said is not None
    # Its owner or failed verdict holds it whatever CI is still doing; only
    # a land waits for CI.
    held = asked and judge_said in ("owner", "failed")
    fixed = dep == "minor" and not gate and (renovate_edited(repo, pr, st) or gate_edit(repo, pr, st))
    swept = fixed and ci == "red" and renovate

    # Dependency bumps: CI is the review. Renovate merges its own minor
    # ones; renovate-sweep fixes the red ones (and settles their threads) and
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
        elif threads or ci == "red":
            what = threads or "CI is red (" + ", ".join(ci_names) + ")"
            kind = "handed" if threads else "redFixed"
            settle("pending", f"Dependency update; {what}"[:140])
            idle = C.NOW - max(ts(pr["updatedAt"]), st["seen"])
            if idle > rc["handoffAfter"] * MIN:
                hand_off(what, kind)
        elif dep == "major" and renovate and major_verdict != "ok":
            # renovate-sweep's verdict comes first, and its job runs the proofs
            # until then: neither a judge nor a proof job of the steward's
            # works the branch beside it.
            settle("pending", "Major update; renovate-sweep held it: see its comment" if major_verdict == "held"
                   else "Major update; waiting for renovate-sweep's verdict on this head")
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
            # Renovate merges its own; Dependabot's need merging.
            if not renovate and st.get("landed") != head:
                merge()
        # Renovate's stuck ones are renovate-sweep's; a Dependabot one that
        # still hasn't merged after a day is the owner's to look at.
        if (not renovate and C.NOW - ts(pr["createdAt"]) > 86400
                and not any(x[1] == line for x in digest)):
            digest.append(("Open over a day", line, f"Dependency update, CI {ci}"))
        save(key, st)
        return

    verdict, vtime = codex_verdict(pr, head, st["seen"])
    # DeepSeek, where it's on, is a second reviewer: both have to find nothing.
    use_ds = C.DS["enable"] and rc["deepseek"]
    dsv, dstime, ds_rounds = D.deepseek_verdict(pr, head) if use_ds else ("clean", 0, 0)
    codex_rounds = sum(1 for r in pr["reviews"]["nodes"] if (r["author"] or {}).get("login") == CODEX) - st["base"]
    ds_rounds -= st["dsBase"]
    if st.get("escalated") and CFG["labels"]["owner"] not in labels:
        # The owner took the label off: another full set of rounds.
        st["base"] += codex_rounds
        st["dsBase"] += ds_rounds
        codex_rounds = ds_rounds = 0
        st["escalated"] = False
    rounds = max(codex_rounds, ds_rounds)
    reviewers = "Codex and DeepSeek" if use_ds else "Codex"
    last_ask = max(st["asked"]) if st["asked"] else 0

    def ask(why):
        if len(st["asked"]) >= 4:
            return False
        log(f"{tag}: asking Codex ({why})")
        C.comment(repo, n, f"@codex review\n\n{why}")
        st["asked"].append(C.NOW)
        return True

    # DeepSeek's reviews are queued for its own service (`deepseek` below),
    # which posts each one; this pass only reads what came back. One is on
    # its way while it's queued and no review of this head is newer.
    dst = st["ds"]
    item = f"{key[:-5]}-{head[:12]}.json"
    if use_ds:
        back = load(f"{D.DONE}/{item}", None)
        if back and not DRY:
            os.remove(os.path.join(STATE, D.DONE, item))
            dst.pop("queued", None)
            if back.get("result") == "tooBig":
                dst["tooBig"] = True
            else:  # the API, the network, its JSON: tried again next pass
                dst["fails"], dst["failedAt"] = dst.get("fails", 0) + 1, C.NOW
                log(f"{tag}: DeepSeek review failed ({dst['fails']}): {back.get('error', '')[:200]}")
        if dst.get("queued") and dstime >= dst["queued"]:
            dst.pop("queued")  # it's back
            dst.pop("gone", None)
        elif dst.get("queued") and not os.path.exists(os.path.join(STATE, D.QUEUE, item)):
            # Out of the queue with nothing back. However long DeepSeek
            # thinks, the item stays queued until its review is posted, so
            # this is a lost one; seen on two passes, since the first may
            # have read the PR just before the review went up.
            if dst.get("gone"):
                dst.pop("queued")
                dst.pop("gone")
                dst["fails"], dst["failedAt"] = dst.get("fails", 0) + 1, C.NOW
                log(f"{tag}: the queued DeepSeek review left the queue without coming back")
            else:
                dst["gone"] = True
    ds_busy = bool(dst.get("queued"))

    def ask_ds():
        """Queue DeepSeek on this head, unless it's already on its way. Its
        looks at one head are bounded by `rounds`: each one with findings
        counts as a round."""
        if ds_busy:
            return
        if DRY:
            log(f"  would: ask DeepSeek to review {tag} at {head[:7]}")
            return
        if dst.get("fails", 0) >= 3 and C.NOW - dst.get("failedAt", 0) > 3600:
            dst["fails"] = 0  # an outage passes: three more tries an hour on
        if dst.get("fails", 0) >= 3 or dst.get("tooBig"):
            return
        log(f"{tag}: queueing a DeepSeek review of {head[:7]}")
        save(f"{D.QUEUE}/{item}", {"repo": repo, "number": n, "headRefOid": head,
                                   "baseRefName": pr["baseRefName"], "title": pr["title"], "at": C.NOW})
        dst["queued"] = C.NOW

    # 1. The reviewers.
    if rounds >= rc["rounds"] and not (verdict == "clean" and dsv == "clean"):
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
    waiting = []
    if verdict in ("none", "failed") or (verdict == "running" and C.NOW - vtime > rc["codexTimeout"] * MIN):
        if quiet and (not last_ask or C.NOW - last_ask > rc["codexTimeout"] * MIN):
            ask("Review this head." if not last_ask else "Retrying: the last request got no result.")
        if len(st["asked"]) >= 2 and C.NOW - last_ask > rc["codexTimeout"] * MIN:
            digest.append(("Codex isn't answering", line, f"asked {len(st['asked'])} times on {head[:7]}"))
        waiting.append("Waiting for Codex's review of this commit")
    elif verdict == "running":
        waiting.append("Codex is reviewing this commit")
    # DeepSeek is queued in the same pass as Codex is asked.
    if dsv == "none":
        if quiet:
            ask_ds()
        waiting.append("DeepSeek is reviewing this commit" if dst.get("queued") else "Waiting for DeepSeek's review of this commit")
    # A first look or a second one that can't happen: the owner hears either way.
    ds_stuck = None
    if use_ds and dst.get("tooBig"):
        ds_stuck = f"too big for DeepSeek (over {C.DS['maxDiffChars']} characters of diff)"
        digest.append(("Too big for DeepSeek to review", line, f"over {C.DS['maxDiffChars']} characters of diff"))
    elif use_ds and dst.get("fails", 0) >= 3:
        ds_stuck = f"DeepSeek failed {dst['fails']} times on {head[:7]}"
        digest.append(("DeepSeek couldn't review it", line, f"failed {dst['fails']} times on {head[:7]} (journalctl -u autoland-deepseek)"))

    what = []
    if open_threads:
        what.append(f"{open_threads} open review thread{'s' * (open_threads > 1)}")
    if changes:
        what.append("changes requested by " + ", ".join(changes))
    idle = C.NOW - max(ts(pr["updatedAt"]), st["seen"])
    if waiting:
        settle("pending", "; ".join(waiting + what))
        # One reviewer's threads needn't wait for the other to finish.
        if what and idle > rc["handoffAfter"] * MIN and not swept:
            hand_off(", ".join(what))
    elif "findings" in (verdict, dsv) or open_threads or changes:
        if not open_threads and not changes:
            # Every finding answered without a new commit: the reviewers that
            # had findings look again.
            if verdict == "findings" and vtime > last_ask and C.NOW - vtime > rc["settleDelay"] * MIN:
                ask("Every finding is answered and resolved; review again.")
            if dsv == "findings" and C.NOW - dstime > rc["settleDelay"] * MIN:
                ask_ds()
            settle("pending", f"Findings answered; {ds_stuck}" if dsv == "findings" and ds_stuck
                   else f"Findings answered; review round {rounds + 1} next")
        else:
            settle("pending", f"Round {rounds}: " + ", ".join(what))
            if idle > rc["handoffAfter"] * MIN and not swept:
                hand_off(", ".join(what))
    # 2. The reviewers are clean on this head and nothing is open: CI, then land.
    elif ci == "red":
        settle("pending", "CI is red: " + ", ".join(ci_names)[:100])
        # Its own once a head, apart from a findings job's: one that answered
        # a finding here without pushing didn't fix a check that failed since.
        if idle > rc["handoffAfter"] * MIN and not swept:
            hand_off("CI is red (" + ", ".join(ci_names) + ")", "redFixed")
    elif held:
        judged("a Claude job was asked to judge this head")
    elif proof_only:
        prove()
    elif ci == "pending":
        settle("pending", "Settled; waiting for CI: " + ", ".join(ci_names)[:90])
    elif gate or ci == "none" or asked:
        judged(f"it touches {gate}" if gate else "the repo has no CI" if ci == "none"
               else "a Claude job was asked to judge this head")
    else:
        land_now(f"{reviewers} found nothing on this commit, every thread is resolved, CI is green")

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
    if sys.argv[1:] == ["deepseek"]:
        for d in (D.QUEUE, D.DONE):
            os.makedirs(os.path.join(STATE, d), exist_ok=True)
        D.deepseek_queue()
        return
    try:
        for d in ("", D.QUEUE, D.DONE):
            os.makedirs(os.path.join(STATE, d), exist_ok=True)
    except OSError:
        if not DRY:
            raise
    repos = R.discover()
    # Repos whose baseline job has started. A repo not in it gets one, a few
    # per pass so the first pass on a machine doesn't start them all at once.
    known = set(load("known.json", []))
    # Ones whose job didn't start: tried again a day later, so they don't
    # take every pass's slots.
    failed = load("baseline-failed.json", {})
    started = 0
    digest = []
    covered = []
    for r in repos:
        repo = r["repo"]
        try:
            new = R.settle_repo(r, known)
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
            heads = set()
            seen_to = True
            for pr in prs:
                heads.add(pr["headRefName"])
                try:
                    handle(repo, pr, info, digest)
                except Exception as e:  # one bad PR mustn't stop the rest
                    log(f"{repo}#{pr['number']}: {e}")
                    seen_to = False
            if seen_to:
                covered.append(r)
            if CFG["cleanBranches"]:
                R.sweep_branches(r, heads, digest)
        except Exception as e:
            log(f"{repo}: {e}")
    # For renovate-sweep: it lists the ok majors of the repos this pass didn't
    # see to (one whose pass failed among them), and tells its jobs how every
    # repo's proofs run.
    save("repos.json", coverage(repos, covered))
    save("known.json", sorted(known))
    save("baseline-failed.json", failed)
    R.publish(digest)


if __name__ == "__main__":
    main()
