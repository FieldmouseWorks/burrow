"""The repos: finding them, keeping their settings at the baseline, the
daily branch sweep, and the one issue that lists what waits for the owner."""

import fnmatch
import json
import os
import subprocess
import tempfile

import common as C
from common import CFG, CONTEXT, DEP_RE, DRY, load, log, repo_cfg, save, ts


def discover():
    seen = []
    for owner in CFG["owners"]:
        for r in C.gh("repo", "list", owner, "--no-archived", "--source", "--limit", "100000",
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
    if C.NOW - st.get("settings", 0) < 86400:
        return new
    info = C.gh("api", f"repos/{repo}")
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
        after = C.write("api", "-X", "PATCH", f"repos/{repo}", "--input", "-", input=json.dumps(patch), check=False)
        # A private repo on a free account takes allow_auto_merge without an
        # error and leaves it off (the steward merges its PRs itself): once the
        # PATCH went through and it's still off, stop asking every day.
        if "allow_auto_merge" in patch and isinstance(after, dict) and not after.get("allow_auto_merge"):
            st["autoMergeRefused"] = True
    C.write("api", "-X", "PUT", f"repos/{repo}/vulnerability-alerts", check=False)

    if CFG["rulesets"] and rc["ruleset"]:
        existing = C.gh("api", f"repos/{repo}/rulesets", check=False)
        # None: the plan doesn't offer rules on this repo (private, free account).
        if isinstance(existing, list) and not any(x["name"] == "baseline" for x in existing):
            log(f"{repo}: adding the baseline ruleset")
            C.write("api", "-X", "POST", f"repos/{repo}/rulesets", "--input", "-",
                    input=json.dumps(baseline_ruleset(rc, r["default"])), check=False)

    st["settings"] = C.NOW
    save(f"repo-{repo.replace('/', '_')}.json", st)
    return new


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
    if C.NOW - st.get("branches", 0) < 86400:
        for b in st.get("stale", []):
            digest.append(("Stale branches (unmerged, no PR, 30+ days)", f"{repo}: `{b}`", ""))
        return
    stale = []
    # The machine's globs, plus the repo's own (evidence branches it cites).
    keep = CFG["keepBranches"] + repo_cfg(repo).get("keepBranches", [])
    names = (C.gh("api", "--paginate", f"repos/{repo}/branches?per_page=100", "--jq",
                  '.[] | select(.protected | not) | "\\(.name) \\(.commit.sha)"', check=False) or "")
    for x in str(names).split("\n"):
        name, _, sha = x.partition(" ")
        if not name or not sha or name == r["default"] or name in open_heads or DEP_RE.match(name):
            continue
        if any(fnmatch.fnmatch(name, pat) for pat in keep):
            continue
        # Compared at the tip just listed, and deleted only if it's still there.
        cmp = C.gh("api", f"repos/{repo}/compare/{r['default']}...{sha}", "--jq",
                   "{ahead: .ahead_by, date: .commits[-1].commit.committer.date}", check=False)
        if not isinstance(cmp, dict):
            continue
        if cmp["ahead"] == 0:
            log(f"{repo}: deleting {name} at {sha[:7]} (already in {r['default']})")
            delete_branch(repo, name, sha)
        elif cmp.get("date") and C.NOW - ts(cmp["date"]) > 30 * 86400:
            stale.append(name)
    st.update(branches=C.NOW, stale=stale)
    save(f"repo-{repo.replace('/', '_')}.json", st)
    for b in stale:
        digest.append(("Stale branches (unmerged, no PR, 30+ days)", f"{repo}: `{b}`", ""))


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
    if prev.get("body") == body and C.NOW - prev.get("checked", 0) < 3600:
        return
    found = C.gh("issue", "list", "-R", nrepo, "--label", label, "--state", "open", "--search",
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
            ok = C.gh("issue", "close", str(found[0]["number"]), "-R", nrepo, "--comment", "Nothing waiting.", check=False) is not None
        if ok:
            save("digest.json", {"lines": [], "body": body, "checked": C.NOW})
        return
    full = body + "\n\n_Kept up to date by autoland-steward on " + os.uname().nodename + "._"
    if not found:
        C.gh("label", "create", label, "-R", nrepo, "--color", "B60205", check=False)
        args = ["issue", "create", "-R", nrepo, "--label", label, "--title", title, "--body", full]
        if CFG["notify"]["assignee"]:
            args += ["--assignee", CFG["notify"]["assignee"]]
        ok = C.gh(*args, check=False) is not None
    else:
        num = str(found[0]["number"])
        if found[0]["body"] != full:
            ok = C.gh("issue", "edit", num, "-R", nrepo, "--body", full, check=False) is not None
        added = [x for x in lines if x not in prev["lines"]]
        if added and ok:
            ok = C.gh("issue", "comment", num, "-R", nrepo, "--body", "New:\n" + "\n".join(f"- {x}" for x in added), check=False) is not None
    if ok:
        save("digest.json", {"lines": lines, "body": body, "checked": C.NOW})
