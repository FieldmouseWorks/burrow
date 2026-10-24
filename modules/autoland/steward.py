"""autoland-steward: every PR in every repo the owner has, from draft to merged.

One pass (the timer runs it every couple of minutes):

  1. Find the repos: every non-archived, non-fork repo of each owner in
     `owners`, new ones included. Bring each one's settings to the baseline
     (auto-merge, delete-branch-on-merge, squash, secret scanning, the
     `baseline` ruleset, the hold and owner labels); a repo seen for the first time also gets a Claude
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
import tempfile
import time
from datetime import datetime

CFG = json.load(open(os.environ["AUTOLAND_CONFIG"]))
CONTEXT = CFG["context"]
CODEX = CFG["codexLogin"]
DRY = len(sys.argv) > 1 and sys.argv[1] == "status"
# A dry run keeps its own state, so its once-a-day reads (settings, branches)
# stay once a day instead of running every pass on the owner's API quota.
STATE = CFG["stateDir"] + ("/dry" if DRY else "")
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
    path = os.path.join(STATE, name)
    try:
        with open(path + ".tmp", "w") as f:
            json.dump(value, f, indent=1)
        os.replace(path + ".tmp", path)
    except OSError:
        if not DRY:  # `status` from a shell that can't write the state dir: fine
            raise


def repo_cfg(repo):
    """The defaults, with this repo's overrides from config.repos on top."""
    c = dict(CFG["defaults"])
    c.update(CFG["repos"].get(repo, {}))
    return c


# ---------------------------------------------------------------- repos

def discover():
    seen = []
    for owner in CFG["owners"]:
        for r in gh("repo", "list", owner, "--no-archived", "--source", "--limit", "100000",
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


LABELS = {"hold": ("FBCA04", "autoland: paused, nothing merges it"),
          "owner": ("B60205", "autoland: waits for the owner")}


def ensure_labels(repo):
    """Create the labels the steward reads (config.labels) where the repo
    lacks them. True once all of them are there."""
    have = gh("label", "list", "-R", repo, "--limit", "1000", "--json", "name", check=False)
    if not isinstance(have, list):
        return False
    names = {x["name"].lower() for x in have}  # GitHub matches labels case-blind
    ok = True
    for key, (color, desc) in LABELS.items():
        name = CFG["labels"][key]
        if name.lower() not in names:
            log(f"{repo}: creating the {name} label")
            ok = write("label", "create", name, "-R", repo, "--color", color,
                       "--description", desc, check=False) is not None and ok
    return ok


def settle_repo(r, known):
    """The repo's settings, at most once a day. Returns True if it's new."""
    repo, rc = r["repo"], repo_cfg(r["repo"])
    key = f"repo-{repo.replace('/', '_')}.json"
    st = load(key, {})
    new = repo not in known
    # A repo seen for the first time has no state yet, so it's settled now.
    due = NOW - st.get("settings", 0) >= 86400
    # The labels are looked at with the settings, and straight away when
    # config.labels names ones not yet made here.
    wanted = sorted(CFG["labels"].values())
    if (due or st.get("labels") != wanted) and ensure_labels(repo):
        st["labels"] = wanted
        save(key, st)
    if not due:
        return new
    info = gh("api", f"repos/{repo}")
    patch = {}
    for key in ("allow_auto_merge", "delete_branch_on_merge", "allow_squash_merge"):
        if not info.get(key) and not (key == "allow_auto_merge" and st.get("autoMergeRefused")):
            patch[key] = True
    if not r["private"]:
        sa = info.get("security_and_analysis") or {}
        for key in ("secret_scanning", "secret_scanning_push_protection"):
            if (sa.get(key) or {}).get("status") != "enabled":
                patch.setdefault("security_and_analysis", {})[key] = {"status": "enabled"}
    if patch:
        log(f"{repo}: settings {sorted(patch)}")
        after = write("api", "-X", "PATCH", f"repos/{repo}", "--input", "-", input=json.dumps(patch), check=False)
        # A private repo on a free account takes allow_auto_merge without an
        # error and leaves it off (the steward merges its PRs itself): once the
        # PATCH went through and it's still off, stop asking every day.
        if "allow_auto_merge" in patch and isinstance(after, dict) and not after.get("allow_auto_merge"):
            st["autoMergeRefused"] = True
    write("api", "-X", "PUT", f"repos/{repo}/vulnerability-alerts", check=False)

    if CFG["rulesets"] and rc["ruleset"]:
        existing = gh("api", f"repos/{repo}/rulesets", check=False)
        # None: the plan doesn't offer rules on this repo (private, free account).
        if isinstance(existing, list) and not any(x["name"] == "baseline" for x in existing):
            log(f"{repo}: adding the baseline ruleset")
            write("api", "-X", "POST", f"repos/{repo}/rulesets", "--input", "-",
                  input=json.dumps(baseline_ruleset(rc, r["default"])), check=False)

    st["settings"] = NOW
    save(key, st)
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
        id number title url isDraft isCrossRepository headRefName headRefOid baseRefName
        createdAt updatedAt mergeStateStatus
        author { login }
        autoMergeRequest { enabledAt }
        labels(first: 100) { pageInfo { hasNextPage } nodes { name } }
        files(first: 100) { nodes { path } }
        changedFiles
        reviewThreads(first: 100) { pageInfo { hasNextPage } nodes { isResolved } }
        reviews(last: 100) { pageInfo { hasPreviousPage } nodes { author { login } state body submittedAt commit { oid } } }
        comments(last: 100) { nodes { databaseId body createdAt updatedAt author { login } } }
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


def ci_state(pr):
    """green / pending / red / none (no checks at all) over every check on
    the head except ours, and the names of the ones that aren't green."""
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


ACTION_PINS_RE = re.compile(r"^\.github/workflows/[^/]+\.ya?ml$")
# A changed line that only repins an action: "uses: owner/action@sha # v4.1.0".
PIN_LINE_RE = re.compile(r"^[+-]\s*(-\s+)?uses:\s*([^\s#@]+)@[^\s#]+(\s+#.*)?$")


def pins_only(repo, pr):
    """True if every changed line in the PR's workflow files only repins an
    action: each removed `uses:` comes back with the same action at a new ref,
    nothing added or dropped (no permissions, triggers or commands). None if
    the diff couldn't be read."""
    diff = gh("pr", "diff", str(pr["number"]), "-R", repo, check=False)
    if not isinstance(diff, str) or not diff:
        return None
    path, changed = None, {}
    for x in diff.splitlines():
        if x.startswith("diff --git "):
            path = x.split(" b/", 1)[-1]
        elif x.startswith(("+++", "---")):
            continue
        elif path and ACTION_PINS_RE.match(path) and x[:1] in "+-" and x.strip("+- \t"):
            m = PIN_LINE_RE.match(x)
            if not m:
                return False
            changed.setdefault(path, {"+": [], "-": []})[x[0]].append(m.group(2))
    return all(sorted(c["+"]) == sorted(c["-"]) for c in changed.values())


def one_way(repo, pr, rc, dep=None):
    """The first path (or label) that makes this PR the owner's to merge."""
    for lab in pr["labels"]["nodes"]:
        if lab["name"] == CFG["labels"]["owner"]:
            return f"the {lab['name']} label"
    # A dependency bot that only repins actions in workflows is a dependency
    # update: CI is its review (unless actionBumps is off).
    pins = None
    for f in pr["files"]["nodes"]:
        if dep and rc["actionBumps"] and ACTION_PINS_RE.match(f["path"]):
            if pins is None:
                # One diff per head and base, not one per pass (retargeting
                # changes the diff); a failed read isn't kept, so the next
                # pass reads it again.
                cache = load("pins.json", {})
                key = f"{repo}#{pr['number']}"
                seen = [pr["headRefOid"], pr["baseRefName"]]
                if (cache.get(key) or [None])[:2] == seen:
                    pins = cache[key][2]
                else:
                    pins = pins_only(repo, pr)
                    if pins is not None:
                        cache[key] = seen + [pins]
                        save("pins.json", cache)
                    pins = bool(pins)
            if pins:
                continue
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


def renovate_edited(repo, pr, st):
    """Whether a Renovate branch has a commit Renovate didn't write (a Claude
    job's fix). Renovate won't merge a branch edited like that. Asked once
    per head; a failed ask counts as not edited and is asked again."""
    if not CFG["renovateEmail"] or not pr["headRefName"].startswith("renovate/"):
        return False
    if st.get("editedHead") != pr["headRefOid"]:
        got = gh("pr", "view", str(pr["number"]), "-R", repo, "--json", "commits", check=False)
        if not got:
            return False
        st["edited"] = any(a.get("email") != CFG["renovateEmail"]
                           for c in got["commits"] for a in c["authors"])
        st["editedHead"] = pr["headRefOid"]
    return st["edited"]


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
        # Only the head that was checked: a push since then waits for the next pass.
        method = [merge_method(repo_info), "--match-head-commit", pr["headRefOid"]]
        if write("pr", "merge", n, "-R", repo, "--auto", *method, check=False) is None and not DRY:
            # No branch rules to wait on: GitHub won't queue it, so merge it now.
            return write("pr", "merge", n, "-R", repo, *method, check=False) is not None
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

    def update_branch():
        """Bring a settled head up to date with a base whose rules want it
        (auto-merge won't). The merged-in head is reviewed like any other."""
        if pr.get("mergeStateStatus") != "BEHIND" or st.get("updated") == head:
            return False
        if write("api", "-X", "PUT", f"repos/{repo}/pulls/{n}/update-branch",
                 "-f", f"expected_head_sha={head}", check=False) is None:
            return False
        log(f"{tag}: behind its base, brought up to date")
        st["updated"] = head
        return True

    def readied():
        """Codex reviews a draft again when it's marked ready, so a PR made
        ready and merged in one go gets a review after the merge. Ready it
        first, then merge or list it a pass later, once that review has
        had its chance: a finding it posts unsettles the PR like any other.
        It stays ready (Codex reviews a ready PR's new commits by itself),
        so it's readied once, not every time it settles again."""
        if not pr["isDraft"]:
            return True
        if write("pr", "ready", str(n), "-R", repo, check=False) is not None:
            log(f"{tag}: settled, marked ready for Codex's last look")
        return False

    def owner_merges(desc):
        if update_branch():
            settle("pending", "Settled, but behind its base: brought up to date, reviewed again")
        elif not readied():
            settle("pending", f"{desc}; marked ready, Codex takes a last look first"[:140])
        else:
            settle("pending", desc, "Settled, yours to merge")

    def hand_off(why):
        # Recorded only once a job has started; until then it's tried every
        # pass and listed for the owner.
        jid = job(repo, fix_task(repo, pr, why))
        if jid:
            st["handed"] = head
            log(f"{tag}: handed to a Claude job {jid} ({why})")
        else:
            log(f"{tag}: couldn't hand to a Claude job (no checkout, or proj refused)")
            digest.append(("Couldn't start a Claude job to fix it", line, why))

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

    # A list longer than one page could hide a hold, an open thread or a
    # changes-requested review: fail closed.
    cut = [k for k, more in (("labels", pr["labels"]["pageInfo"]["hasNextPage"]),
                             ("review threads", pr["reviewThreads"]["pageInfo"]["hasNextPage"]),
                             ("reviews", pr["reviews"]["pageInfo"]["hasPreviousPage"])) if more]
    if cut:
        settle("pending", f"Over 100 {' and '.join(cut)}, too many to read: {CFG['owner']} merges it", "Settled, yours to merge")
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

    ci, ci_names = ci_state(pr)
    # No checks ran at all. Unless the repo is set not to need any (docs
    # only), that's not green: nothing has tested it, so the owner merges it.
    no_ci = ci == "none" and rc["requireCi"]
    if ci == "none":
        ci, ci_names = ("green", []) if not rc["requireCi"] else ("pending", ["no CI on this repo yet"])
    open_threads = sum(1 for t in pr["reviewThreads"]["nodes"] if not t["isResolved"])
    latest = {}
    for r in pr["reviews"]["nodes"]:
        who = (r["author"] or {}).get("login")
        if who and who != CODEX and r["state"] in ("APPROVED", "CHANGES_REQUESTED", "DISMISSED"):
            latest[who] = r["state"]
    changes = [w for w, s in latest.items() if s == "CHANGES_REQUESTED"]
    dep = dep_kind(pr)
    gate = one_way(repo, pr, rc, dep)
    # A minor Renovate PR a Claude job fixed carries Claude's code, and Renovate
    # won't merge it: it goes through the review loop below like a Claude PR.
    # While its CI is red it's renovate-sweep's to hand over, findings and all:
    # a job from each on one branch would push over each other. The sweep's
    # push makes a new head, and Codex reads it again.
    fixed = dep == "minor" and not gate and renovate_edited(repo, pr, st)
    swept = fixed and ci == "red"

    # Dependency bumps: CI is the review. Renovate merges its own;
    # renovate-sweep handles the red ones and the majors.
    if dep and not fixed:
        if gate:
            owner_merges(f"Dependency update; {CFG['owner']} merges it (touches {gate})")
        elif dep == "major":
            # Renovate's majors are listed by renovate-sweep; Dependabot's here.
            why = None if pr["headRefName"].startswith("renovate/") else "Major dependency updates"
            settle("pending", f"Major update: {CFG['owner']} merges it", why)
        elif ci == "green" and not open_threads and not changes:
            settle("success", "Dependency update, CI green")
            # Renovate merges its own; Dependabot's need merging.
            if not pr["headRefName"].startswith("renovate/") and st.get("landed") != head:
                merge()
        elif changes:
            settle("pending", "Dependency update; changes requested by " + ", ".join(changes))
        else:
            settle("pending", f"Dependency update, CI {ci}")
        # Renovate's stuck ones are renovate-sweep's; a Dependabot one that
        # still hasn't merged after a day is the owner's to look at.
        if (not gate and dep != "major" and not pr["headRefName"].startswith("renovate/")
                and NOW - ts(pr["createdAt"]) > 86400):
            digest.append(("Open over a day", line, f"Dependency update, CI {ci}"))
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
            # Escalated only once the label is on: one that isn't reads next
            # pass as the owner taking it off, and the rounds start over.
            add = ("pr", "edit", str(n), "-R", repo, "--add-label", CFG["labels"]["owner"])
            if write(*add, check=False) is not None or (ensure_labels(repo) and write(*add, check=False) is not None):
                comment(repo, n, f"{rounds} Codex rounds and it still has findings, so this stops here and waits for "
                                 f"{CFG['owner']}. Taking the `{CFG['labels']['owner']}` label off gives it another {rc['rounds']}.")
                st["escalated"] = True
            else:
                log(f"{tag}: couldn't add the {CFG['labels']['owner']} label; trying again next pass")
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
            if idle > rc["handoffAfter"] * MIN and st.get("handed") != head and not swept:
                hand_off(", ".join(what))
    # 2. Codex is clean on this head and nothing is open: CI, then land.
    elif ci == "red":
        settle("pending", "CI is red: " + ", ".join(ci_names)[:100])
        idle = NOW - max(ts(pr["updatedAt"]), st["seen"])
        if idle > rc["handoffAfter"] * MIN and st.get("handed") != head and not swept:
            hand_off("CI is red (" + ", ".join(ci_names) + ")")
    elif no_ci:
        owner_merges(f"Settled, but this repo has no CI: {CFG['owner']} merges it")
    elif ci == "pending" and all(n in rc["ownerChecks"] for n in ci_names):
        # Only checks a person clears are left (a verdict from a test
        # machine): waiting quietly would never end, so it's the owner's.
        then = f"then merges it (touches {gate})" if gate else "then it lands"
        desc = f"Settled; {CFG['owner']} clears {', '.join(ci_names)}, {then}"[:140]
        if gate:
            owner_merges(desc)
        else:
            settle("pending", desc, "Settled, yours to merge")
    elif ci == "pending":
        settle("pending", "Settled; waiting for CI: " + ", ".join(ci_names)[:90])
    elif gate:
        owner_merges(f"Settled; {CFG['owner']} merges it (touches {gate})")
    else:
        settle("success", "Codex found nothing on this commit, every thread is resolved, CI is green")
        if st.get("landed") != head:
            if readied():
                log(f"{tag}: settled, landing")
                merge()
        else:
            update_branch()

    # Anything not merged after a day, Codex clean or not (a check that never
    # reports keeps it pending). One-way PRs, and those already listed for
    # the owner (no CI), are left out.
    if NOW - ts(pr["createdAt"]) > 86400 and not gate and not any(x[1] == line for x in digest):
        digest.append(("Open over a day", line, our_status(pr)[1] or "unsettled"))
    save(key, st)


# ---------------------------------------------------------------- branches

def delete_branch(repo, name, sha):
    """Delete the branch only if its tip is still `sha`: a push since then
    keeps it (git's lease; the REST API has no conditional delete)."""
    if DRY:
        log(f"  would: delete {repo} {name} if still at {sha[:7]}")
        return
    with tempfile.TemporaryDirectory() as d:
        subprocess.run(["git", "init", "-q", "--bare", d], check=False)
        p = subprocess.run(["git", "-C", d, "push", "-q", f"--force-with-lease=refs/heads/{name}:{sha}",
                            f"https://github.com/{repo}.git", f":refs/heads/{name}"],
                           capture_output=True, text=True)
        if p.returncode != 0:
            log(f"{repo}: kept {name}: {p.stderr.strip()[:200]}")


def sweep_branches(r, open_heads, digest):
    repo = r["repo"]
    st = load(f"repo-{repo.replace('/', '_')}.json", {})
    if NOW - st.get("branches", 0) < 86400:
        for b in st.get("stale", []):
            digest.append(("Stale branches (unmerged, no PR, 30+ days)", f"{repo}: `{b}`", ""))
        return
    stale = []
    # The machine's globs, plus the repo's own (evidence branches it cites).
    keep = CFG["keepBranches"] + repo_cfg(repo).get("keepBranches", [])
    names = (gh("api", "--paginate", f"repos/{repo}/branches?per_page=100", "--jq",
                '.[] | select(.protected | not) | "\\(.name) \\(.commit.sha)"', check=False) or "")
    for x in str(names).split("\n"):
        name, _, sha = x.partition(" ")
        if not name or not sha or name == r["default"] or name in open_heads or DEP_RE.match(name):
            continue
        if any(fnmatch.fnmatch(name, pat) for pat in keep):
            continue
        # Compared at the tip just listed, and deleted only if it's still there.
        cmp = gh("api", f"repos/{repo}/compare/{r['default']}...{sha}", "--jq",
                 "{ahead: .ahead_by, date: .commits[-1].commit.committer.date}", check=False)
        if not isinstance(cmp, dict):
            continue
        if cmp["ahead"] == 0:
            log(f"{repo}: deleting {name} at {sha[:7]} (already in {r['default']})")
            delete_branch(repo, name, sha)
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
    prev = load("digest.json", {"lines": []})
    # Unchanged since the last look, under an hour ago: nothing to read or write.
    if prev.get("body") == body and NOW - prev.get("checked", 0) < 3600:
        return
    found = gh("issue", "list", "-R", nrepo, "--label", label, "--state", "open", "--search",
               f'"{title}" in:title', "--json", "number,body", check=False)
    if not isinstance(found, list):
        return  # couldn't look (no quota?): try next pass, and don't open a second issue
    found = [i for i in found if i]
    lines = sorted({line for _, line, _ in digest})
    # Recorded as published only once every call went through, so a failed
    # one is tried again next pass rather than an hour later.
    ok = True
    if not digest:
        if found:
            ok = gh("issue", "close", str(found[0]["number"]), "-R", nrepo, "--comment", "Nothing waiting.", check=False) is not None
        if ok:
            save("digest.json", {"lines": [], "body": body, "checked": NOW})
        return
    full = body + "\n\n_Kept up to date by autoland-steward on " + os.uname().nodename + "._"
    if not found:
        gh("label", "create", label, "-R", nrepo, "--color", "B60205", check=False)
        args = ["issue", "create", "-R", nrepo, "--label", label, "--title", title, "--body", full]
        if CFG["notify"]["assignee"]:
            args += ["--assignee", CFG["notify"]["assignee"]]
        ok = gh(*args, check=False) is not None
    else:
        num = str(found[0]["number"])
        if found[0]["body"] != full:
            ok = gh("issue", "edit", num, "-R", nrepo, "--body", full, check=False) is not None
        added = [x for x in lines if x not in prev["lines"]]
        if added and ok:
            ok = gh("issue", "comment", num, "-R", nrepo, "--body", "New:\n" + "\n".join(f"- {x}" for x in added), check=False) is not None
    if ok:
        save("digest.json", {"lines": lines, "body": body, "checked": NOW})


# ---------------------------------------------------------------- main

def main():
    try:
        os.makedirs(STATE, exist_ok=True)
    except OSError:
        if not DRY:
            raise
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
