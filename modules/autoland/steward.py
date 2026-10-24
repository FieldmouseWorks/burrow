"""autoland-steward: every PR in every repo the owner has, from draft to merged.

One pass (the timer runs it every couple of minutes):

  1. Find the repos: every non-archived, non-fork repo of each owner in
     `owners`, new ones included. Bring each one's settings to the baseline
     (auto-merge, delete-branch-on-merge, squash, secret scanning, the
     `baseline` ruleset); a repo seen for the first time also gets a Claude
     job that brings its files to docs/repo-baseline.md.
  2. For each open PR, work out where it stands and do the next thing:
     ask Codex for a review (and queue one from DeepSeek, if it's on:
     config.deepseek),
     hand open findings or red CI to a Claude job,
     post the `review-settled` commit status, mark it ready and turn on
     auto-merge, or stop and list it for the owner.
  3. Once a day per repo, delete branches whose tip is already in the
     default branch, and list the stale unmerged ones.
  4. Keep one issue up to date with everything waiting for the owner.

Everything talks to GitHub through `gh`, with the user's own login, so
DeepSeek's reviews are posted under it too, each marked as DeepSeek's. Settings
are in the JSON file $AUTOLAND_CONFIG (../autoland.nix). State is in
config.stateDir: one JSON file per PR, one per repo.

`autoland-steward status` prints what it thinks of every open PR without
changing anything.

`autoland-steward deepseek` works through the DeepSeek reviews the passes
queued (config.stateDir/ds-queue), a few at once, and posts each one. A
careful look takes DeepSeek many minutes, so it runs as its own service and
no pass waits for it.
"""

import fnmatch
import json
import os
import re
import ssl
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime

CFG = json.load(open(os.environ["AUTOLAND_CONFIG"]))
CONTEXT = CFG["context"]
CODEX = CFG["codexLogin"]
DS = CFG.get("deepseek") or {"enable": False}
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


def settle_repo(r, known):
    """The repo's settings, at most once a day. Returns True if it's new."""
    repo, rc = r["repo"], repo_cfg(r["repo"])
    st = load(f"repo-{repo.replace('/', '_')}.json", {})
    new = repo not in known
    # A repo seen for the first time has no state yet, so it's settled now.
    if NOW - st.get("settings", 0) < 86400:
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
        "For every unresolved review thread (Codex's, DeepSeek's, which the steward posts under the owner's login marked DeepSeek, or anyone's): "
        "either fix it, push, and reply on the thread with the commit, "
        "or reply with why it's wrong or out of scope; then resolve the thread (gh api graphql, resolveReviewThread). "
        "If CI is red, read the failing job's log, fix the cause and push. Run the repo's tests before you push. "
        "Don't ask for a review, don't mark the PR ready and don't merge: the steward does those once you're done. "
        "If something needs the owner (a decision, a secret, a one-way change), comment on the PR saying exactly what, "
        f"add the {CFG['labels']['owner']} label, and stop."
    )


# ---------------------------------------------------------------- DeepSeek

# The steward posts DeepSeek's reviews under the user's own login, so they're
# told apart by these markers: the review's names the commit and how many
# findings it had, and each finding's thread carries the other.
DS_RE = re.compile(r"<!-- deepseek-review:v1 (\{.*?\}) -->")
DS_FINDING = "<!-- deepseek-finding -->"
QUEUE, DONE = "ds-queue", "ds-done"  # under STATE: asked for, and what went wrong
STALE = 4 * 3600  # a queued review not back by then is taken as failed
_me = []


def me():
    """The login the steward runs as (and posts DeepSeek's reviews as)."""
    if not _me:
        _me.append(gh("api", "user", "--jq", ".login"))
    return _me[0]


def deepseek_verdict(pr, head):
    """What DeepSeek last said about this exact head (clean, findings or
    none), when, and how many of its reviews on the PR had findings."""
    verdict, at, rounds = "none", 0, 0
    for r in pr["reviews"]["nodes"]:
        m = DS_RE.search(r["body"] or "")
        if not m or (r["author"] or {}).get("login") != me():
            continue
        try:
            found = int(json.loads(m.group(1)).get("findings", 0)) > 0
            sha = json.loads(m.group(1)).get("commit_sha")
        except (ValueError, TypeError, AttributeError):
            continue
        rounds += found
        if sha == head and (r["commit"] or {}).get("oid") == head and ts(r["submittedAt"]) >= at:
            verdict, at = ("findings" if found else "clean"), ts(r["submittedAt"])
    return verdict, at, rounds


QUOTED = r'"(?:[^"\\]|\\.)*"'


def unquote(t):
    """A name as git writes it: bare, or quoted with C-style escapes (the
    octal ones bytes of UTF-8) when it holds unusual characters. None if
    it's neither."""
    if not t.startswith('"'):
        return t
    if not re.fullmatch(QUOTED, t):
        return None
    try:
        return t[1:-1].encode("latin-1", "backslashreplace").decode("unicode_escape").encode("latin-1").decode("utf-8", "replace")
    except (UnicodeError, ValueError):
        return None


def header_pairs(header):
    """Every (old, new) a `diff --git` line can be read as. Git leaves
    spaces bare, so "a/x b/y b/z" is two renames at once; the caller
    checks them all."""
    rest = header[len("diff --git "):]
    pairs = []
    for i, c in enumerate(rest):
        if c == " ":
            a, b = unquote(rest[:i]), unquote(rest[i + 1:])
            if a and b and a.startswith("a/") and b.startswith("b/"):
                pairs.append((a[2:], b[2:]))
    return pairs


def file_names(lines):
    """For one file's part of a diff: every name it might have (old or
    new, however its header splits), and the one it has on the new side
    (the old one if it's deleted), or None if that's not certain."""
    pairs = header_pairs(lines[0])
    names = {x for p in pairs for x in p}
    told = {}
    for x in lines[1:]:
        if x.startswith("@@"):
            break
        m = re.match(r"(rename|copy) (from|to) (.*)", x) or re.match(r"(---|\+\+\+) (?:(/dev/null)|(.*?))\t?$", x)
        if not m:
            continue
        if m.group(2) == "/dev/null":
            told[m.group(1)] = None
            continue
        name = unquote(m.group(3))
        if m.group(1) in ("---", "+++"):
            name = name[2:] if name and name[:2] in ("a/", "b/") else None
        if name is None:
            return names, None  # a name it can't read
        names.add(name)
        told[m.group(1) + (m.group(2) if m.group(1) in ("rename", "copy") else "")] = name
    same = {b for a, b in pairs if a == b}
    path = (told.get("+++") or told.get("renameto") or told.get("copyto")
            or (told.get("---") if "+++" in told else None)
            or (same.pop() if len(same) == 1 else pairs[0][1] if len(pairs) == 1 else None))
    return names, path


def numbered(diff):
    """The diff, file by file, with each new-side line's number in front (the
    numbers findings point at), and the lines a review comment can go on."""
    files, out = {}, []
    skip = [re.compile(p) for p in DS["skipPaths"]]
    for part in re.split(r"(?m)^(?=diff --git )", diff):
        lines = part.splitlines()
        if not lines or not lines[0].startswith("diff --git "):
            continue
        names, path = file_names(lines)
        # A rename out of a secrets path still carries its contents, and a
        # file whose name isn't certain is hidden rather than risked.
        hidden = path is None or any(p.search(q) for p in skip for q in names)
        path = path or lines[0]
        files[path] = set()
        out.append(f"\n=== {path}" + ("\n(contents not sent: a secrets path)" if hidden else ""))
        if hidden:
            continue
        new, in_hunk = 0, False
        for x in lines[1:]:
            if x.startswith("@@"):
                m = re.match(r"@@ -\d+(?:,\d+)? \+(\d+)", x)
                new, in_hunk = (int(m.group(1)) if m else 0), True
                out.append(x)
            elif not in_hunk:
                # The file's header: only the lines that say what happened to it.
                if x.startswith(("new file", "deleted file", "old mode", "new mode", "rename ", "copy ", "Binary files")):
                    out.append(x)
            elif x.startswith("-"):
                out.append(f"{'':>6} {x}")
            elif x.startswith(("+", " ")):
                files[path].add(new)
                out.append(f"{new:>6} {x}")
                new += 1
            else:
                out.append(f"{'':>6} {x}")  # "\ No newline at end of file"
    return "\n".join(out), files


def review_rules(repo, base, paths):
    """The `## Code Review Rules` of the AGENTS.md nearest each changed file,
    as the base branch has them (a PR can't rewrite its own rules)."""
    tree = gh("api", f"repos/{repo}/git/trees/{base}?recursive=1", "--jq",
              '[.tree[] | select(.type == "blob") | .path | select(test("(^|/)AGENTS\\\\.md$"))]', check=False)
    have = set(tree) if isinstance(tree, list) else set()
    wanted = set()
    for p in paths:
        d = p
        while d:
            d = d.rpartition("/")[0]
            f = f"{d}/AGENTS.md" if d else "AGENTS.md"
            if f in have:
                wanted.add(f)
                break
    rules = []
    for f in sorted(wanted):
        text = gh("api", "-H", "Accept: application/vnd.github.raw", f"repos/{repo}/contents/{f}?ref={base}", check=False)
        m = re.search(r"^## Code Review Rules\s*$(.*?)(?=^## |\Z)", text if isinstance(text, str) else "", re.M | re.S)
        if m:
            rules.append(f"From {f}:\n{m.group(1).strip()}")
    return "\n\n".join(rules)


def prior_findings(repo, n):
    """DeepSeek's earlier findings on this PR and the replies each got."""
    out = gh("api", "--paginate", f"repos/{repo}/pulls/{n}/comments?per_page=100", "--jq",
             ".[] | {id, in_reply_to_id, path, line, original_line, body, user: .user.login}", check=False)
    cs = []
    for x in (out if isinstance(out, str) else json.dumps(out) if out else "").splitlines():
        try:
            cs.append(json.loads(x))
        except ValueError:
            pass
    roots = [c for c in cs if c["user"] == me() and DS_FINDING in (c["body"] or "")]
    text = []
    for c in roots:
        replies = [r for r in cs if r.get("in_reply_to_id") == c["id"]]
        body = c["body"].replace(DS_FINDING, "").strip()
        at = c.get("line") or c.get("original_line")
        text.append(f"- {c['path']}{f':{at}' if at else ''}: {body}\n"
                    + "".join(f"  Reply from {r['user']}: {r['body'].strip()}\n" for r in replies))
    return "\n".join(text)[:60000]


# The answer's shape. In strict mode (DeepSeek's beta endpoint) the API
# holds the model to it: every property required, nothing else allowed.
DS_REVIEW_SCHEMA = {
    "type": "object",
    "properties": {
        "findings": {"type": "array", "items": {
            "type": "object",
            "properties": {
                "path": {"type": "string"},
                "line": {"type": "integer"},
                "severity": {"type": "string", "enum": ["P0", "P1", "P2"]},
                "title": {"type": "string"},
                "body": {"type": "string"},
            },
            "required": ["path", "line", "severity", "title", "body"],
            "additionalProperties": False,
        }},
        "summary": {"type": "string"},
    },
    "required": ["findings", "summary"],
    "additionalProperties": False,
}


def ask_deepseek(system, user):
    """One review from DeepSeek, as a dict. Raises on any failure.

    First as a strict tool call, which the API holds to DS_REVIEW_SCHEMA; if
    that's turned down (it's a beta), JSON mode, which now and then answers
    with something that isn't JSON (its docs say so, and remi saw one in
    six); if that's turned down too, plain text. Each answer that can't be
    read is asked again, three tries in all.

    Its thinking counts against max_tokens. At max effort a big diff can
    think past even that (remi#39: most calls did, after 11 minutes each),
    and asking the same again would only do it again: it's asked once more
    at high effort, and if that runs out too the review fails."""
    env = {}
    for x in open(os.path.join(os.environ.get("CREDENTIALS_DIRECTORY", ""), "deepseek")):
        k, sep, v = x.strip().partition("=")
        if sep:
            env[k] = v.strip().strip('"')
    efforts = [env.get("DEEPSEEK_EFFORT") or DS["effort"]]
    if efforts[0] == "max":
        efforts.append("high")
    base = {
        "model": env["DEEPSEEK_MODEL"],
        "messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
        "max_tokens": DS["maxTokens"],
    }
    url = DS["baseUrl"].rstrip("/")
    ways = [
        (url + "/beta", {
            "tools": [{"type": "function", "function": {
                "name": "submit_review", "strict": True,
                "description": "Submit the review: every finding, and a one-sentence summary.",
                "parameters": DS_REVIEW_SCHEMA}}],
            # Thinking mode turns down a forced or "required" tool_choice
            # (remi#34); with "auto" it called submit_review every time.
            "tool_choice": "auto",
        }),
        (url, {"response_format": {"type": "json_object"}}),
        (url, {}),
    ]
    ctx = ssl.create_default_context(cafile=DS["caFile"])

    def call(at, extra):
        body = {**base, **extra, "reasoning_effort": efforts[0]}
        req = urllib.request.Request(at + "/chat/completions", data=json.dumps(body).encode(),
                                     headers={"Authorization": f"Bearer {env['DEEPSEEK_API_KEY']}",
                                              "Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=DS["timeout"], context=ctx) as r:
            choice = json.load(r)["choices"][0]
        msg = choice["message"]
        for t in msg.get("tool_calls") or []:
            if t["function"]["name"] == "submit_review":
                return choice.get("finish_reason"), t["function"]["arguments"] or ""
        return choice.get("finish_reason"), msg.get("content") or ""

    tries = 0
    while True:
        at, extra = ways[0]
        try:
            why, text = call(at, extra)
        except urllib.error.HTTPError as e:
            if e.code != 400 or len(ways) == 1:
                raise
            log(f"  DeepSeek turned down {', '.join(extra) or 'plain text'} ({e.code}): trying the next way")
            ways.pop(0)
            continue
        if why == "length":
            if len(efforts) == 1:
                raise RuntimeError(f"thought past {DS['maxTokens']} tokens at {efforts[0]} effort without answering")
            log(f"  DeepSeek thought past {DS['maxTokens']} tokens at {efforts[0]} effort: asking at {efforts[1]}")
            efforts.pop(0)
            continue
        tries += 1
        try:
            return json.loads(text[text.index("{"):text.rindex("}") + 1])
        except ValueError:
            if tries == 3:
                raise RuntimeError(f"no JSON in its answer: {text[:100]!r}")


DS_SYSTEM = """You review pull requests for {owner}'s repos. You are one of two reviewers; a PR merges itself once both find nothing on its latest commit, so what you let through ships.

Report only real defects a careful senior reviewer would block the merge for: bugs, security holes, data loss, races, broken error handling, tests or checks weakened to get green, behaviour changed beyond what the PR says it does, and anything the repo's Code Review Rules flag. No style or naming nits, no praise, no "consider" suggestions, no restating the diff, nothing you can't point at in the code. If nothing qualifies, return no findings: that is the usual outcome for a good commit.

Earlier findings on this PR are listed with the replies they got. Don't raise one again unless the code still has the problem and the reply is wrong; then say why the reply doesn't hold.

Each finding: "path" (as the diff names it), "line" (a number from the diff's left column, on the new side, where the problem is), "severity" ("P0": unsafe or breaks things, "P1": a real bug or a rule broken, "P2": a smaller real defect), "title" (one line), "body" (what is wrong, why, and the fix, in a few sentences). At most 10, most severe first.

Answer by calling submit_review if you have it, otherwise with JSON only: {{"findings": [...], "summary": "one sentence on the PR as a whole"}}"""


def quiet_mentions(text):
    """Model text goes out under the user's login: no @-pings from it."""
    return re.sub(r"@(?=[\w-])", "@​", str(text)).replace("<!--", "&lt;!--")


def run_deepseek(repo, pr):
    """Ask DeepSeek about the head and post what it says as one review.
    Returns "clean", "findings", or "tooBig" (nothing sent). Raises if it
    didn't happen."""
    n, head = pr["number"], pr["headRefOid"]
    diff = gh("api", "-H", "Accept: application/vnd.github.diff",
              f"repos/{repo}/compare/{pr['baseRefName']}...{head}", check=False)
    if not isinstance(diff, str) or not diff.startswith("diff"):
        raise RuntimeError("couldn't read the diff")
    text, files = numbered(diff)
    if len(text) > DS["maxDiffChars"]:
        return "tooBig"
    user = (f"Repo {repo}, PR #{n}: {pr['title']}\n\n"
            f"Code Review Rules:\n{review_rules(repo, pr['baseRefName'], files) or '(none)'}\n\n"
            f"Earlier findings:\n{prior_findings(repo, n) or '(none)'}\n\n"
            f"The diff against {pr['baseRefName']}:\n{text}")
    log(f"{repo}#{n}: asking DeepSeek ({len(text)} chars of diff)")
    answer = ask_deepseek(DS_SYSTEM.format(owner=CFG["owner"]), user)
    # Anything but a list of findings is no review at all, never a clean one.
    found = answer.get("findings") if isinstance(answer, dict) else None
    if not isinstance(found, list) or not all(isinstance(f, dict) for f in found):
        raise RuntimeError(f"not a review: {json.dumps(answer)[:100]}")
    comments, whole_file, loose = [], [], []
    for f in found[:10]:
        sev = f.get("severity") if f.get("severity") in ("P0", "P1", "P2") else "P1"
        msg = f"**DeepSeek · {sev}** {quiet_mentions(f.get('title', ''))}\n\n{quiet_mentions(f.get('body', ''))}"
        lines = files.get(f.get("path"))
        if lines is None:
            # Not a file this PR changes: a note in the review, not a finding.
            loose.append(f"- `{quiet_mentions(f.get('path'))}`: " + msg.replace("\n\n", " "))
            continue
        if not lines:
            # Changed, but with no new side to comment on (deleted, binary,
            # a secrets path): a thread on the whole file, still a finding.
            whole_file.append({"path": f["path"], "body": msg + "\n\n" + DS_FINDING})
            continue
        try:
            want = int(f.get("line"))
        except (TypeError, ValueError):
            want = min(lines)
        line = want if want in lines else min(lines, key=lambda x: abs(x - want))
        comments.append({"path": f["path"], "line": line, "side": "RIGHT", "body": msg + "\n\n" + DS_FINDING})
    k = len(comments) + len(whole_file)
    head_line = (f"**DeepSeek review** of {head[:7]}: "
                 + (f"{k} finding{'s' * (k > 1)}. Fix or answer each, then resolve its thread." if k else "nothing found."))
    body = head_line + (f"\n\n{quiet_mentions(answer.get('summary', ''))}" if answer.get("summary") else "")
    if loose:
        body += "\n\nNotes not on a changed line:\n" + "\n".join(loose)
    body += "\n\n<!-- deepseek-review:v1 " + json.dumps({"commit_sha": head, "findings": k}) + " -->"
    posted = gh("api", "-X", "POST", f"repos/{repo}/pulls/{n}/reviews", "--input", "-",
                input=json.dumps({"commit_id": head, "event": "COMMENT", "body": body, "comments": comments}), check=False)
    if not isinstance(posted, dict):
        raise RuntimeError("GitHub didn't take the review")
    # A review can't hold file-level comments: they go on their own. The
    # review already counts them, so a lost one still keeps the PR unsettled.
    for c in whole_file:
        if not isinstance(gh("api", "-X", "POST", f"repos/{repo}/pulls/{n}/comments", "--input", "-",
                             input=json.dumps({**c, "commit_id": head, "subject_type": "file"}), check=False), dict):
            log(f"{repo}#{n}: couldn't post DeepSeek's finding on {c['path']}")
    log(f"{repo}#{n}: DeepSeek review posted, {k} finding(s)")
    return "findings" if k else "clean"


def handle(repo, pr, repo_info, digest):
    rc = repo_cfg(repo)
    n, head = pr["number"], pr["headRefOid"]
    key = f"pr-{repo.replace('/', '_')}-{n}.json"
    st = load(key, {})
    if st.get("head") != head:
        st.update(head=head, seen=NOW, asked=[], handed=None, landFails=0, ds={})
    st.setdefault("base", 0)
    st.setdefault("dsBase", 0)
    st.setdefault("ds", {})
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

    # Dependency bumps: CI is the review. Renovate merges its own;
    # renovate-sweep handles the red ones and the majors.
    if dep:
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
    # DeepSeek, where it's on, is a second reviewer: both have to find nothing.
    use_ds = DS["enable"] and rc["deepseek"]
    dsv, dstime, ds_rounds = deepseek_verdict(pr, head) if use_ds else ("clean", 0, 0)
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
        comment(repo, n, f"@codex review\n\n{why}")
        st["asked"].append(NOW)
        return True

    # DeepSeek's reviews are queued for its own service (`deepseek` below),
    # which posts each one; this pass only reads what came back. One is on
    # its way while it's queued and no review of this head is newer.
    dst = st["ds"]
    item = f"{key[:-5]}-{head[:12]}.json"
    if use_ds:
        back = load(f"{DONE}/{item}", None)
        if back and not DRY:
            os.remove(os.path.join(STATE, DONE, item))
            dst.pop("queued", None)
            if back.get("result") == "tooBig":
                dst["tooBig"] = True
            else:  # the API, the network, its JSON: tried again next pass
                dst["fails"], dst["failedAt"] = dst.get("fails", 0) + 1, NOW
                log(f"{tag}: DeepSeek review failed ({dst['fails']}): {back.get('error', '')[:200]}")
        if dst.get("queued") and dstime >= dst["queued"]:
            dst.pop("queued")  # it's back
        elif dst.get("queued") and NOW - dst["queued"] > STALE:
            dst.pop("queued")
            dst["fails"], dst["failedAt"] = dst.get("fails", 0) + 1, NOW
            log(f"{tag}: the DeepSeek review queued over {STALE // 3600}h ago never came back")
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
        if dst.get("fails", 0) >= 3 and NOW - dst.get("failedAt", 0) > 3600:
            dst["fails"] = 0  # an outage passes: three more tries an hour on
        if dst.get("fails", 0) >= 3 or dst.get("tooBig"):
            return
        log(f"{tag}: queueing a DeepSeek review of {head[:7]}")
        save(f"{QUEUE}/{item}", {"repo": repo, "number": n, "headRefOid": head,
                                 "baseRefName": pr["baseRefName"], "title": pr["title"], "at": NOW})
        dst["queued"] = NOW

    # 1. The reviewers.
    if rounds >= rc["rounds"] and not (verdict == "clean" and dsv == "clean"):
        if not st.get("escalated"):
            write("pr", "edit", str(n), "-R", repo, "--add-label", CFG["labels"]["owner"], check=False)
            comment(repo, n, f"{rounds} review rounds and there are still findings, so this stops here and waits for "
                             f"{CFG['owner']}. Taking the `{CFG['labels']['owner']}` label off gives it another {rc['rounds']}.")
            st["escalated"] = True
        settle("pending", f"Still unsettled after {rounds} review rounds: {CFG['owner']} decides", "Unsettled after the round limit")
        save(key, st)
        return

    quiet = age >= rc["settleDelay"] * MIN
    waiting = []
    if verdict in ("none", "failed") or (verdict == "running" and NOW - vtime > rc["codexTimeout"] * MIN):
        if quiet and (not last_ask or NOW - last_ask > rc["codexTimeout"] * MIN):
            ask("Review this head." if not last_ask else "Retrying: the last request got no result.")
        if len(st["asked"]) >= 2 and NOW - last_ask > rc["codexTimeout"] * MIN:
            digest.append(("Codex isn't answering", line, f"asked {len(st['asked'])} times on {head[:7]}"))
        waiting.append("Waiting for Codex's review of this commit")
    elif verdict == "running":
        waiting.append("Codex is reviewing this commit")
    # DeepSeek is queued in the same pass as Codex is asked.
    if dsv == "none":
        if quiet:
            ask_ds()
        if dst.get("tooBig"):
            digest.append(("Too big for DeepSeek to review", line, f"over {DS['maxDiffChars']} characters of diff"))
        elif dst.get("fails", 0) >= 3:
            digest.append(("DeepSeek couldn't review it", line, f"failed {dst['fails']} times on {head[:7]} (journalctl -u autoland-deepseek)"))
        waiting.append("DeepSeek is reviewing this commit" if dst.get("queued") else "Waiting for DeepSeek's review of this commit")

    what = []
    if open_threads:
        what.append(f"{open_threads} open review thread{'s' * (open_threads > 1)}")
    if changes:
        what.append("changes requested by " + ", ".join(changes))
    idle = NOW - max(ts(pr["updatedAt"]), st["seen"])
    if waiting:
        settle("pending", "; ".join(waiting + what))
        # One reviewer's threads needn't wait for the other to finish.
        if what and idle > rc["handoffAfter"] * MIN and st.get("handed") != head:
            hand_off(", ".join(what))
    elif "findings" in (verdict, dsv) or open_threads or changes:
        if not open_threads and not changes:
            # Every finding answered without a new commit: the reviewers that
            # had findings look again.
            if verdict == "findings" and vtime > last_ask and NOW - vtime > rc["settleDelay"] * MIN:
                ask("Every finding is answered and resolved; review again.")
            if dsv == "findings" and NOW - dstime > rc["settleDelay"] * MIN:
                ask_ds()
            settle("pending", f"Findings answered; review round {rounds + 1} next")
        else:
            settle("pending", f"Round {rounds}: " + ", ".join(what))
            if idle > rc["handoffAfter"] * MIN and st.get("handed") != head:
                hand_off(", ".join(what))
    # 2. The reviewers are clean on this head and nothing is open: CI, then land.
    elif ci == "red":
        settle("pending", "CI is red: " + ", ".join(ci_names)[:100])
        idle = NOW - max(ts(pr["updatedAt"]), st["seen"])
        if idle > rc["handoffAfter"] * MIN and st.get("handed") != head:
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
        settle("success", f"{reviewers} found nothing on this commit, every thread is resolved, CI is green")
        if st.get("landed") != head:
            if readied():
                log(f"{tag}: settled, landing")
                merge()
        else:
            update_branch()

    # Anything not merged after a day, reviewed clean or not (a check that never
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


# ---------------------------------------------------------------- DeepSeek's queue

def review_queued(name):
    """One queued review: posted, or what went wrong left in DONE for the
    next pass. Gone from the queue either way."""
    item = load(f"{QUEUE}/{name}", None)
    repo, n = (item or {}).get("repo"), (item or {}).get("number")
    try:
        if not item:
            return
        cur = gh("api", f"repos/{repo}/pulls/{n}", "--jq", "{state, sha: .head.sha}")
        if cur.get("state") != "open" or cur.get("sha") != item["headRefOid"]:
            log(f"{repo}#{n}: moved on from {item['headRefOid'][:7]}, not reviewing it")
            return
        if run_deepseek(repo, item) == "tooBig":
            save(f"{DONE}/{name}", {"result": "tooBig", "at": time.time()})
    except Exception as e:
        log(f"{repo}#{n}: DeepSeek review failed: {str(e)[:300]}")
        save(f"{DONE}/{name}", {"result": "error", "error": str(e)[:300], "at": time.time()})
    finally:
        try:
            os.remove(os.path.join(STATE, QUEUE, name))
        except OSError:
            pass


def deepseek_queue():
    """Every queued review, `parallel` at a time, until the queue is empty
    (passes may add to it meanwhile)."""
    q = os.path.join(STATE, QUEUE)
    # What went wrong on a head no pass came back for (it moved on).
    for name in os.listdir(os.path.join(STATE, DONE)):
        path = os.path.join(STATE, DONE, name)
        if time.time() - os.path.getmtime(path) > 86400:
            os.remove(path)
    with ThreadPoolExecutor(DS["parallel"]) as pool:
        busy, seen = [], set()
        while True:
            for name in sorted(os.listdir(q)):
                if name.endswith(".json") and name not in seen:
                    seen.add(name)
                    busy.append(pool.submit(review_queued, name))
            busy = [f for f in busy if not f.done()]
            if not busy:
                return
            time.sleep(5)


# ---------------------------------------------------------------- main

def main():
    if sys.argv[1:] == ["deepseek"]:
        for d in (QUEUE, DONE):
            os.makedirs(os.path.join(STATE, d), exist_ok=True)
        deepseek_queue()
        return
    try:
        for d in ("", QUEUE, DONE):
            os.makedirs(os.path.join(STATE, d), exist_ok=True)
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
