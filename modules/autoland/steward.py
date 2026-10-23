"""autoland-steward: every PR in every repo the owner has, from draft to merged.

One pass (the timer runs it every couple of minutes):

  1. Find the repos: every non-archived, non-fork repo of each owner in
     `owners`, new ones included. Bring each one's settings to the baseline
     (auto-merge, delete-branch-on-merge, squash, secret scanning, the
     `baseline` ruleset); a repo seen for the first time also gets a Claude
     job that brings its files to docs/repo-baseline.md.
  2. For each open PR, work out where it stands and do the next thing:
     ask Codex for a review, hand open findings or red CI to a Claude job,
     post the `review-settled` commit status, mark it ready and turn on
     auto-merge, or stop and list it for the owner.
  3. Once a day per repo, delete branches whose tip is already in the
     default branch, and list the stale unmerged ones.
  4. Keep one issue up to date with everything waiting for the owner.

Everything talks to GitHub through `gh`, with the user's own login. Settings
are in the JSON file $AUTOLAND_CONFIG (../autoland.nix). State is in
config.stateDir: one JSON file per PR, one per repo.

`autoland-steward status` prints what it thinks of every open PR without
changing anything.
"""

import fnmatch
import json
import os
import re
import subprocess
import sys
import time
from datetime import datetime

CFG = json.load(open(os.environ["AUTOLAND_CONFIG"]))
STATE = CFG["stateDir"]
CONTEXT = CFG["context"]
CODEX = CFG["codexLogin"]
DRY = len(sys.argv) > 1 and sys.argv[1] == "status"
NOW = time.time()
MIN = 60


def log(*a):
    print(*a, flush=True)


def gh(*args, input=None, check=True):
    """Run gh; return its stdout (parsed as JSON when it is JSON)."""
    p = subprocess.run(["gh", *args], input=input, capture_output=True, text=True)
    if p.returncode != 0:
        if check:
            raise RuntimeError(f"gh {' '.join(args[:3])}: {p.stderr.strip()[:300]}")
        return None
    out = p.stdout.strip()
    try:
        return json.loads(out) if out else ""
    except ValueError:
        return out


def write(*args, **kw):
    """A gh call that changes something: skipped (and logged) in status mode."""
    if DRY:
        log("  would:", "gh", " ".join(a if len(a) < 80 else a[:77] + "..." for a in args))
        return None
    return gh(*args, **kw)


def graphql(query, **variables):
    args = ["api", "graphql", "-f", f"query={query}"]
    for k, v in variables.items():
        if v is not None:
            args += ["-F" if isinstance(v, int) else "-f", f"{k}={v}"]
    return gh(*args)["data"]


def open_prs(owner, name):
    """The repo's merge settings and every open PR, all pages."""
    prs, cursor = [], None
    while True:
        info = graphql(PRS, owner=owner, name=name, cursor=cursor)["repository"]
        page = info.pop("pullRequests")
        prs += page["nodes"]
        if not page["pageInfo"]["hasNextPage"]:
            return info, prs
        cursor = page["pageInfo"]["endCursor"]


def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp() if s else 0


def load(name, default):
    try:
        return json.load(open(os.path.join(STATE, name)))
    except (OSError, ValueError):
        return default


def save(name, value):
    if DRY:
        return
    path = os.path.join(STATE, name)
    with open(path + ".tmp", "w") as f:
        json.dump(value, f, indent=1)
    os.replace(path + ".tmp", path)


def repo_cfg(repo):
    """The defaults, with this repo's overrides from config.repos on top."""
    c = dict(CFG["defaults"])
    c.update(CFG["repos"].get(repo, {}))
    return c


# ---------------------------------------------------------------- repos

def discover():
    seen = []
    for owner in CFG["owners"]:
        for r in gh("repo", "list", owner, "--no-archived", "--source", "--limit", "1000",
                    "--json", "nameWithOwner,isPrivate,viewerPermission,defaultBranchRef") or []:
            name = r["nameWithOwner"]
            if any(fnmatch.fnmatch(name, pat) for pat in CFG["exclude"]):
                continue
            if r["viewerPermission"] != "ADMIN" or not r.get("defaultBranchRef"):
                continue
            seen.append({"repo": name, "private": r["isPrivate"], "default": r["defaultBranchRef"]["name"]})
    return seen


def baseline_ruleset(rc, default):
    rules = [{"type": "deletion"}, {"type": "non_fast_forward"}]
    if not rc["directPush"]:
        rules += [
            {"type": "pull_request", "parameters": {
                "required_approving_review_count": 0,
                "dismiss_stale_reviews_on_push": False,
                # CODEOWNERS (if any) names the paths the owner signs off on.
                "require_code_owner_review": True,
                "require_last_push_approval": False,
                "required_review_thread_resolution": True,
            }},
            {"type": "required_status_checks", "parameters": {
                "strict_required_status_checks_policy": False,
                "required_status_checks": [{"context": CONTEXT}],
            }},
        ]
    return {
        "name": "baseline",
        "target": "branch",
        "enforcement": "active",
        "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
        # The owner (an admin) can still merge past it, from the PR page.
        "bypass_actors": [{"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "pull_request"}]
        if not rc["directPush"] else [{"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always"}],
        "rules": rules,
    }


def settle_repo(r, known):
    """The repo's settings, at most once a day. Returns True if it's new."""
    repo, rc = r["repo"], repo_cfg(r["repo"])
    st = load(f"repo-{repo.replace('/', '_')}.json", {})
    new = repo not in known
    if NOW - st.get("settings", 0) < 86400 and not new:
        return new
    info = gh("api", f"repos/{repo}")
    patch = {}
    for key in ("allow_auto_merge", "delete_branch_on_merge", "allow_squash_merge"):
        if not info.get(key):
            patch[key] = True
    if not r["private"]:
        sa = info.get("security_and_analysis") or {}
        for key in ("secret_scanning", "secret_scanning_push_protection"):
            if (sa.get(key) or {}).get("status") != "enabled":
                patch.setdefault("security_and_analysis", {})[key] = {"status": "enabled"}
    if patch:
        log(f"{repo}: settings {sorted(patch)}")
        write("api", "-X", "PATCH", f"repos/{repo}", "--input", "-", input=json.dumps(patch), check=False)
    write("api", "-X", "PUT", f"repos/{repo}/vulnerability-alerts", check=False)

    if CFG["rulesets"] and rc["ruleset"]:
        existing = gh("api", f"repos/{repo}/rulesets", check=False)
        # None: the plan doesn't offer rules on this repo (private, free account).
        if isinstance(existing, list) and not any(x["name"] == "baseline" for x in existing):
            log(f"{repo}: adding the baseline ruleset")
            write("api", "-X", "POST", f"repos/{repo}/rulesets", "--input", "-",
                  input=json.dumps(baseline_ruleset(rc, r["default"])), check=False)

    st["settings"] = NOW
    save(f"repo-{repo.replace('/', '_')}.json", st)
    return new


def project_dir(repo):
    """The repo's checkout in projectsDir (by origin URL), or None."""
    src = CFG["projectsDir"]
    for d in sorted(os.listdir(src)) if os.path.isdir(src) else []:
        p = subprocess.run(["git", "-C", os.path.join(src, d), "remote", "get-url", "origin"],
                           capture_output=True, text=True)
        url = re.sub(r"^(https://github\.com/|git@github\.com:)", "", p.stdout.strip()).removesuffix(".git")
        if url.lower() == repo.lower():
            return d
    return None


def job(repo, task, clone=False):
    """Hand a task to a Claude job in the repo's project. Returns the job id."""
    d = project_dir(repo)
    if d is None and clone and not DRY:
        d = repo.split("/")[1]
        subprocess.run(["gh", "repo", "clone", repo, os.path.join(CFG["projectsDir"], d)], check=False)
        d = project_dir(repo)
    if d is None:
        return None
    if DRY:
        log(f"  would: proj run {d} ...")
        return "dry"
    p = subprocess.run([CFG["proj"], "run", d, task], capture_output=True, text=True)
    return p.stdout.strip() or None


# ---------------------------------------------------------------- PRs

PRS = """
query($owner: String!, $name: String!, $cursor: String) {
  repository(owner: $owner, name: $name) {
    squashMergeAllowed mergeCommitAllowed rebaseMergeAllowed
    pullRequests(states: OPEN, first: 30, after: $cursor, orderBy: {field: CREATED_AT, direction: ASC}) {
      pageInfo { hasNextPage endCursor }
      nodes {
        id number title url isDraft isCrossRepository headRefName headRefOid
        createdAt updatedAt
        author { login }
        autoMergeRequest { enabledAt }
        labels(first: 30) { nodes { name } }
        files(first: 100) { nodes { path } }
        changedFiles
        reviewThreads(first: 100) { nodes { isResolved comments(first: 1) { nodes { author { login } } } } }
        reviews(last: 100) { nodes { author { login } state body submittedAt commit { oid } } }
        comments(last: 100) {
          nodes {
            databaseId body createdAt updatedAt author { login }
            reactions(last: 20) { nodes { content createdAt user { login } } }
          }
        }
        reactions(last: 20) { nodes { content createdAt user { login } } }
        commits(last: 1) {
          nodes { commit { oid statusCheckRollup { contexts(first: 100) { nodes {
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
        m = RECEIPT_RE.search(body)
        if m and head.startswith(m.group(1)):
            events.append((ts(c["createdAt"]), "clean" if "find any major issues" in body else "findings"))
        elif "Something went wrong" in body and ts(c["createdAt"]) >= since:
            events.append((ts(c["createdAt"]), "failed"))
    for r in pr["reviews"]["nodes"]:
        if (r["author"] or {}).get("login") == CODEX and (r["commit"] or {}).get("oid") == head:
            clean = "find any major issues" in (r["body"] or "")
            events.append((ts(r["submittedAt"]), "clean" if clean else "findings"))
    # A thumbs-up (clean) or eyes (working on it) on the PR or on a request.
    for holder in [pr] + pr["comments"]["nodes"]:
        for x in (holder.get("reactions") or {}).get("nodes", []):
            if (x["user"] or {}).get("login") == CODEX and ts(x["createdAt"]) >= since:
                if x["content"] == "THUMBS_UP":
                    events.append((ts(x["createdAt"]), "clean"))
                elif x["content"] == "EYES":
                    events.append((ts(x["createdAt"]), "running"))
    if not events:
        return "none", 0
    t, v = max(events)
    return v, t


def ci_state(pr):
    """green / pending / red / none (no checks at all) over every check on
    the head except ours, and the names of the ones that aren't green."""
    nodes = pr["commits"]["nodes"]
    rollup = (nodes[0]["commit"].get("statusCheckRollup") or {}) if nodes else {}
    red, pending, seen = [], [], 0
    for c in (rollup.get("contexts") or {}).get("nodes", []):
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
    write("api", f"repos/{repo}/statuses/{pr['headRefOid']}", "-f", f"state={state}",
          "-f", f"context={CONTEXT}", "-f", f"description={desc}", "-f", f"target_url={pr['url']}", check=False)


def one_way(repo, pr, rc):
    """The first path (or label) that makes this PR the owner's to merge."""
    for lab in pr["labels"]["nodes"]:
        if lab["name"] == CFG["labels"]["owner"]:
            return f"the {lab['name']} label"
    for f in pr["files"]["nodes"]:
        for pat in rc["oneWay"]:
            if re.search(pat, f["path"]):
                return f["path"]
    if pr["changedFiles"] > len(pr["files"]["nodes"]):
        return f"{pr['changedFiles']} files (too many to check)"
    return None


DEP_RE = re.compile(r"^(renovate|dependabot)/")
# "from 1.2.3 to 2.0.0", "from v3 to v4" (an action's major tag), "from 3 to 4".
MAJOR_RE = re.compile(r"from v?(\d+)\S* to v?(\d+)")


def dep_kind(pr):
    """None, 'minor' or 'major' for a dependency bot's PR."""
    if not DEP_RE.match(pr["headRefName"]):
        return None
    labels = {x["name"] for x in pr["labels"]["nodes"]}
    m = MAJOR_RE.search(pr["title"])
    if "major" in labels or (m and m.group(1) != m.group(2)):
        return "major"
    return "minor"


def merge_method(repo_info):
    for flag, method in (("squashMergeAllowed", "--squash"), ("mergeCommitAllowed", "--merge"),
                         ("rebaseMergeAllowed", "--rebase")):
        if repo_info[flag]:
            return method
    return "--merge"


def land(repo, pr, repo_info):
    """Ready, then auto-merge (or merge now if nothing is left to wait for).
    False if GitHub took neither."""
    n = str(pr["number"])
    if pr["isDraft"]:
        write("pr", "ready", n, "-R", repo, check=False)
    if not pr["autoMergeRequest"]:
        method = merge_method(repo_info)
        if write("pr", "merge", n, "-R", repo, "--auto", method, check=False) is None and not DRY:
            # No branch rules to wait on: GitHub won't queue it, so merge it now.
            return write("pr", "merge", n, "-R", repo, method, check=False) is not None
    return True


def unland(repo, pr, back_to_draft):
    """Something new came up after it was settled: stop the merge."""
    n = str(pr["number"])
    if pr["autoMergeRequest"]:
        write("pr", "merge", n, "-R", repo, "--disable-auto", check=False)
    if back_to_draft and not pr["isDraft"]:
        write("pr", "ready", n, "-R", repo, "--undo", check=False)


def comment(repo, n, body):
    return write("api", f"repos/{repo}/issues/{n}/comments", "-f", f"body={body}", check=False)


def fix_task(repo, pr, why):
    return (
        f"PR #{pr['number']} in {repo} (\"{pr['title']}\", branch {pr['headRefName']}) isn't settled: {why}. "
        "Settle it. Check out the branch in your own worktree (git worktree add) under the scratch dir, never in this checkout's main tree. "
        "For every unresolved review thread (Codex's or anyone's): either fix it, push, and reply on the thread with the commit, "
        "or reply with why it's wrong or out of scope; then resolve the thread (gh api graphql, resolveReviewThread). "
        "If CI is red, read the failing job's log, fix the cause and push. Run the repo's tests before you push. "
        "Don't ask Codex for a review, don't mark the PR ready and don't merge: the steward does those once you're done. "
        "If something needs the owner (a decision, a secret, a one-way change), comment on the PR saying exactly what, "
        f"add the {CFG['labels']['owner']} label, and stop."
    )


def handle(repo, pr, repo_info, digest):
    rc = repo_cfg(repo)
    n, head = pr["number"], pr["headRefOid"]
    key = f"pr-{repo.replace('/', '_')}-{n}.json"
    st = load(key, {})
    if st.get("head") != head:
        st.update(head=head, seen=NOW, asked=[], handed=None, landFails=0)
    st.setdefault("base", 0)
    labels = {x["name"] for x in pr["labels"]["nodes"]}
    author = (pr["author"] or {}).get("login", "")
    age = NOW - st["seen"]
    tag = f"{repo}#{n}"
    line = f"[{repo}#{n}]({pr['url']}) {pr['title']}"

    def settle(state, desc, why=None):
        post_status(repo, pr, state, desc)
        if why:
            digest.append((why, line, desc))
        # Not settled (any more): nothing may merge it, and if the steward
        # marked it ready, it goes back to draft.
        if state != "success" and (pr["autoMergeRequest"] or (st.get("landed") and not pr["isDraft"])):
            unland(repo, pr, back_to_draft=bool(st.get("landed")))
            st["landed"] = None

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

    if CFG["labels"]["hold"] in labels:
        log(f"{tag}: on hold")
        save(key, st)
        return
    if pr["isCrossRepository"] or author not in CFG["authors"]:
        settle("pending", f"Outside contribution: {CFG['owner']} reviews and merges it", "Outside contributions")
        save(key, st)
        return

    ci, ci_names = ci_state(pr)
    if ci == "none":
        # No checks ran at all. Unless the repo is set not to need any (docs
        # only), that's not green: nothing has tested it.
        ci, ci_names = ("green", []) if not rc["requireCi"] else ("pending", ["no CI on this repo yet"])
    open_threads = sum(1 for t in pr["reviewThreads"]["nodes"] if not t["isResolved"])
    latest = {}
    for r in pr["reviews"]["nodes"]:
        who = (r["author"] or {}).get("login")
        if who and who != CODEX and r["state"] in ("APPROVED", "CHANGES_REQUESTED", "DISMISSED"):
            latest[who] = r["state"]
    changes = [w for w, s in latest.items() if s == "CHANGES_REQUESTED"]
    dep = dep_kind(pr)
    gate = one_way(repo, pr, rc)

    # Dependency bumps: CI is the review. Renovate merges its own;
    # renovate-sweep handles the red ones and the majors.
    if dep:
        if gate:
            settle("pending", f"Dependency update; {CFG['owner']} merges it (touches {gate})", "Settled, yours to merge")
        elif dep == "major":
            # Renovate's majors are listed by renovate-sweep; Dependabot's here.
            why = None if pr["headRefName"].startswith("renovate/") else "Major dependency updates"
            settle("pending", f"Major update: {CFG['owner']} merges it", why)
        elif ci == "green" and not open_threads:
            settle("success", "Dependency update, CI green")
            # Renovate merges its own; Dependabot's need merging.
            if not pr["headRefName"].startswith("renovate/") and st.get("landed") != head:
                merge()
        else:
            settle("pending", f"Dependency update, CI {ci}")
        save(key, st)
        return

    verdict, vtime = codex_verdict(pr, head, st["seen"])
    rounds = sum(1 for r in pr["reviews"]["nodes"] if (r["author"] or {}).get("login") == CODEX) - st["base"]
    if st.get("escalated") and CFG["labels"]["owner"] not in labels:
        # The owner took the label off: another full set of rounds.
        st["base"] += rounds
        rounds = 0
        st["escalated"] = False
    last_ask = max(st["asked"]) if st["asked"] else 0

    def ask(why):
        if len(st["asked"]) >= 4:
            return False
        log(f"{tag}: asking Codex ({why})")
        comment(repo, n, f"@codex review\n\n{why}")
        st["asked"].append(NOW)
        return True

    # 1. Codex.
    if rounds >= rc["rounds"] and verdict != "clean":
        if not st.get("escalated"):
            write("pr", "edit", str(n), "-R", repo, "--add-label", CFG["labels"]["owner"], check=False)
            comment(repo, n, f"{rounds} Codex rounds and it still has findings, so this stops here and waits for "
                             f"{CFG['owner']}. Taking the `{CFG['labels']['owner']}` label off gives it another {rc['rounds']}.")
            st["escalated"] = True
        settle("pending", f"Still unsettled after {rounds} Codex rounds: {CFG['owner']} decides", "Unsettled after the round limit")
        save(key, st)
        return

    quiet = age >= rc["settleDelay"] * MIN
    if verdict in ("none", "failed") or (verdict == "running" and NOW - vtime > rc["codexTimeout"] * MIN):
        if quiet and (not last_ask or NOW - last_ask > rc["codexTimeout"] * MIN):
            ask("Review this head." if not last_ask else "Retrying: the last request got no result.")
        if len(st["asked"]) >= 2 and NOW - last_ask > rc["codexTimeout"] * MIN:
            digest.append(("Codex isn't answering", line, f"asked {len(st['asked'])} times on {head[:7]}"))
        settle("pending", "Waiting for Codex's review of this commit")
    elif verdict == "running":
        settle("pending", "Codex is reviewing this commit")
    elif verdict == "findings" or open_threads or changes:
        what = []
        if open_threads:
            what.append(f"{open_threads} open review thread{'s' * (open_threads > 1)}")
        if changes:
            what.append("changes requested by " + ", ".join(changes))
        if verdict == "findings" and not open_threads and not changes:
            # Every finding answered without a new commit: Codex looks again.
            if vtime > last_ask and NOW - vtime > rc["settleDelay"] * MIN:
                ask("Every finding is answered and resolved; review again.")
            settle("pending", f"Findings answered; Codex round {rounds + 1} next")
        else:
            settle("pending", f"Round {rounds}: " + ", ".join(what))
            idle = NOW - max(ts(pr["updatedAt"]), st["seen"])
            if idle > rc["handoffAfter"] * MIN and st.get("handed") != head:
                jid = job(repo, fix_task(repo, pr, ", ".join(what)))
                st["handed"] = head
                log(f"{tag}: handed to a Claude job ({jid or 'no checkout'})")
                if jid is None:
                    digest.append(("No checkout on this machine to fix it in", line, ", ".join(what)))
    # 2. Codex is clean on this head and nothing is open: CI, then land.
    elif ci == "red":
        settle("pending", "CI is red: " + ", ".join(ci_names)[:100])
        idle = NOW - max(ts(pr["updatedAt"]), st["seen"])
        if idle > rc["handoffAfter"] * MIN and st.get("handed") != head:
            jid = job(repo, fix_task(repo, pr, "CI is red (" + ", ".join(ci_names) + ")"))
            st["handed"] = head
            log(f"{tag}: red CI handed to a Claude job ({jid or 'no checkout'})")
    elif ci == "pending":
        settle("pending", "Settled; waiting for CI: " + ", ".join(ci_names)[:90])
    elif gate:
        settle("pending", f"Settled; {CFG['owner']} merges it (touches {gate})", "Settled, yours to merge")
    else:
        settle("success", "Codex found nothing on this commit, every thread is resolved, CI is green")
        if st.get("landed") != head:
            log(f"{tag}: settled, landing")
            merge()

    if NOW - ts(pr["createdAt"]) > 86400 and verdict != "clean" and not gate:
        digest.append(("Open over a day", line, our_status(pr)[1] or "unsettled"))
    save(key, st)


# ---------------------------------------------------------------- branches

def sweep_branches(r, open_heads, digest):
    repo = r["repo"]
    st = load(f"repo-{repo.replace('/', '_')}.json", {})
    if NOW - st.get("branches", 0) < 86400:
        for b in st.get("stale", []):
            digest.append(("Stale branches (unmerged, no PR, 30+ days)", f"{repo}: `{b}`", ""))
        return
    stale = []
    names = (gh("api", "--paginate", f"repos/{repo}/branches?per_page=100", "--jq",
                ".[] | select(.protected | not) | .name", check=False) or "")
    for name in str(names).split("\n"):
        if not name or name == r["default"] or name in open_heads or DEP_RE.match(name):
            continue
        if any(fnmatch.fnmatch(name, pat) for pat in CFG["keepBranches"]):
            continue
        cmp = gh("api", f"repos/{repo}/compare/{r['default']}...{name}", "--jq",
                 "{ahead: .ahead_by, date: .commits[-1].commit.committer.date}", check=False)
        if not isinstance(cmp, dict):
            continue
        if cmp["ahead"] == 0:
            log(f"{repo}: deleting {name} (already in {r['default']})")
            write("api", "-X", "DELETE", f"repos/{repo}/git/refs/heads/{name}", check=False)
        elif cmp.get("date") and NOW - ts(cmp["date"]) > 30 * 86400:
            stale.append(name)
    st.update(branches=NOW, stale=stale)
    save(f"repo-{repo.replace('/', '_')}.json", st)
    for b in stale:
        digest.append(("Stale branches (unmerged, no PR, 30+ days)", f"{repo}: `{b}`", ""))


# ---------------------------------------------------------------- digest

def publish(digest):
    """One issue in config.notify.repo listing what waits for the owner."""
    nrepo = CFG["notify"]["repo"]
    groups = {}
    for why, line, desc in digest:
        groups.setdefault(why, []).append(f"- {line}" + (f": {desc}" if desc else ""))
    body = "\n\n".join(f"**{why}**\n" + "\n".join(lines) for why, lines in groups.items())
    if DRY or not nrepo:
        log("--- waiting for the owner ---\n" + (body or "(nothing)"))
        return
    title = "[autoland] PRs waiting for you"
    label = CFG["notify"]["label"]
    found = gh("issue", "list", "-R", nrepo, "--label", label, "--state", "open", "--search",
               f'"{title}" in:title', "--json", "number,body", check=False) or []
    found = [i for i in found if i]
    prev = load("digest.json", {"lines": []})
    lines = sorted({line for _, line, _ in digest})
    if not digest:
        if found:
            gh("issue", "close", str(found[0]["number"]), "-R", nrepo, "--comment", "Nothing waiting.", check=False)
        save("digest.json", {"lines": []})
        return
    full = body + "\n\n_Kept up to date by autoland-steward on " + os.uname().nodename + "._"
    if not found:
        gh("label", "create", label, "-R", nrepo, "--color", "B60205", check=False)
        args = ["issue", "create", "-R", nrepo, "--label", label, "--title", title, "--body", full]
        if CFG["notify"]["assignee"]:
            args += ["--assignee", CFG["notify"]["assignee"]]
        gh(*args, check=False)
    else:
        num = str(found[0]["number"])
        if found[0]["body"] != full:
            gh("issue", "edit", num, "-R", nrepo, "--body", full, check=False)
        added = [x for x in lines if x not in prev["lines"]]
        if added:
            gh("issue", "comment", num, "-R", nrepo, "--body", "New:\n" + "\n".join(f"- {x}" for x in added), check=False)
    save("digest.json", {"lines": lines})


# ---------------------------------------------------------------- main

def main():
    os.makedirs(STATE, exist_ok=True)
    repos = discover()
    # Repos whose baseline job has started. A repo not in it gets one, a few
    # per pass so the first pass on a machine doesn't start them all at once.
    known = set(load("known.json", []))
    # Ones whose job didn't start: tried again a day later, so they don't
    # take every pass's slots.
    failed = load("baseline-failed.json", {})
    started = 0
    digest = []
    for r in repos:
        repo = r["repo"]
        try:
            new = settle_repo(r, known)
            if (new and CFG["baselineJobs"] and started < CFG["baselinePerPass"]
                    and NOW - failed.get(repo, 0) > 86400):
                jid = job(repo, CFG["baselineTask"].replace("REPO", repo), clone=True)
                started += 1
                if jid:
                    log(f"{repo}: baseline job {jid}")
                    known.add(repo)
                    failed.pop(repo, None)
                else:
                    log(f"{repo}: baseline job didn't start (no checkout, or proj refused); trying again tomorrow")
                    failed[repo] = NOW
            elif new and not CFG["baselineJobs"]:
                known.add(repo)
            owner, name = repo.split("/")
            info, prs = open_prs(owner, name)
            heads = set()
            for pr in prs:
                heads.add(pr["headRefName"])
                try:
                    handle(repo, pr, info, digest)
                except Exception as e:  # one bad PR mustn't stop the rest
                    log(f"{repo}#{pr['number']}: {e}")
            if CFG["cleanBranches"]:
                sweep_branches(r, heads, digest)
        except Exception as e:
            log(f"{repo}: {e}")
    save("known.json", sorted(known))
    save("baseline-failed.json", failed)
    publish(digest)


if __name__ == "__main__":
    main()
