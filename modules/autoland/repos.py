"""The repos: finding them, keeping their settings at the baseline, the
daily branch sweep, and the one issue that lists what waits for the owner."""

import fnmatch
import hashlib
import json
import os
import subprocess
import tempfile

import common as C
from common import CFG, CONTEXT, DEP_RE, DRY, load, log, repo_cfg, save, ts
from tasks import RED_BRANCH, red_default_task


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
    due = C.NOW - st.get("settings", 0) >= 86400
    # The labels the steward and its jobs put on PRs: without them a hold
    # can't be set and the round limit can't hand a PR to the owner. Made
    # again with the daily settings too, in case one was deleted.
    want = [CFG["labels"]["hold"], CFG["labels"]["owner"]]
    if st.get("labels") != want or due:
        about = ("The steward leaves this PR alone",
                 f"Waits for {CFG['owner']}; take the label off to let the steward carry on")
        made = [C.write("label", "create", name, "-R", repo, "--force", "--color", color,
                        "--description", text, check=False) is not None
                for name, color, text in zip(want, ("fbca04", "b60205"), about)]
        if all(made):
            log(f"{repo}: labels {', '.join(want)}")
            st["labels"] = want
            save(f"repo-{repo.replace('/', '_')}.json", st)
    hook(repo, st, due)
    # A repo seen for the first time has no state yet, so it's settled now.
    if not due:
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

    # The repo's own, which is all not_strict changes; a repo with more than
    # 100 of them is past anything the steward's baseline sets up.
    existing = C.gh("api", f"repos/{repo}/rulesets?per_page=100&includes_parents=false", check=False)
    # None: the plan doesn't offer rules on this repo (private, free account).
    if isinstance(existing, list):
        if CFG["rulesets"] and rc["ruleset"] and not any(x["name"] == "baseline" for x in existing):
            log(f"{repo}: adding the baseline ruleset")
            C.write("api", "-X", "POST", f"repos/{repo}/rulesets", "--input", "-",
                    input=json.dumps(baseline_ruleset(rc, r["default"])), check=False)
        # Whether or not the repo gets the baseline ruleset, its own rules
        # don't make branches be up to date.
        not_strict(repo, existing)
    not_strict_protection(repo, r["default"])

    st["settings"] = C.NOW
    save(f"repo-{repo.replace('/', '_')}.json", st)
    return new


def not_strict(repo, rulesets):
    """Up-to-date branches off in each of the repo's own branch rulesets
    (docs/repo-baseline.md says why). The listing names a ruleset without
    its rules, so each one is read."""
    for x in rulesets:
        rs = C.gh("api", f"repos/{repo}/rulesets/{x['id']}", check=False)
        if not isinstance(rs, dict) or rs.get("target") != "branch" or rs.get("source_type") != "Repository":
            continue
        rules = rs.get("rules") or []
        strict = [y for y in rules if y.get("type") == "required_status_checks"
                  and (y.get("parameters") or {}).get("strict_required_status_checks_policy")]
        if strict:
            for y in strict:
                y["parameters"]["strict_required_status_checks_policy"] = False
            log(f"{repo}: up-to-date branches off in the {rs.get('name')!r} ruleset")
            C.write("api", "-X", "PUT", f"repos/{repo}/rulesets/{x['id']}", "--input", "-",
                    input=json.dumps({"rules": rules}), check=False)


def not_strict_protection(repo, default):
    """The same in the default branch's classic protection. Its checks are
    read only when the branch says it requires some: the endpoint is a 404
    otherwise."""
    b = C.gh("api", f"repos/{repo}/branches/{default}", check=False)
    checks = ((b.get("protection") or {}).get("required_status_checks") or {}) if isinstance(b, dict) else {}
    if checks.get("enforcement_level", "off") == "off":
        return
    path = f"repos/{repo}/branches/{default}/protection/required_status_checks"
    got = C.gh("api", path, check=False)
    if isinstance(got, dict) and got.get("strict"):
        log(f"{repo}: up-to-date branches off in {default}'s protection")
        C.write("api", "-X", "PATCH", path, "-F", "strict=false", check=False)


def watch_default(r, info, prs, digest):
    """A red default branch: CI failed on what's merged, and every PR is
    built on it. With no merge queue (docs/repo-baseline.md), this is how
    two PRs that broke it together get undone: a Claude job finds the
    cause and opens a PR from RED_BRANCH that reverts or fixes it, which
    lands like any other. Nothing more while that job works or its PR is
    open; a second job if it's still red after (seen on two passes), then
    the owner."""
    repo = r["repo"]
    target = (info.get("defaultBranchRef") or {}).get("target") or {}
    sha, state = target.get("oid"), (target.get("statusCheckRollup") or {}).get("state")
    key = f"repo-{repo.replace('/', '_')}.json"
    st = load(key, {})
    red = st.get("red") or {}
    if state not in ("FAILURE", "ERROR"):
        if red and state == "SUCCESS":
            log(f"{repo}: {r['default']} is green again")
            st.pop("red")
            save(key, st)
        return
    line = f"{repo} {r['default']} at {sha[:7]}"
    if red.get("job") and C.job_state(red["job"]) in (None, "queued", "running"):
        return
    # Its PR is in the loop: the one its task names, from the steward's own
    # authors to this branch. Anyone can open a PR from a branch of that name
    # on a fork, and that one holds nothing up.
    fix = RED_BRANCH + (red.get("at") or "")[:12]
    if red.get("at") and any(pr["headRefName"] == fix and pr["baseRefName"] == r["default"] and C.acted_on(pr)
                             for pr in prs):
        return
    # Its job ended with no PR to show. Seen on two passes: this one may
    # have read the PRs just before the job opened its own.
    if red.get("job") and not red.get("ended"):
        red["ended"] = True
        save(key, st)
        C.wake(repo, C.NOW + 60)
        return
    if red.get("jobs", 0) >= 2:
        digest.append(("The default branch is red", line, "two Claude jobs ended with it still red"))
        return
    jid = C.job(repo, red_default_task(repo, r["default"], sha))
    if not jid:
        digest.append(("Couldn't start a Claude job to fix it", line, "the default branch is red"))
        C.wake(repo, C.NOW + 5 * 60)
        return
    log(f"{repo}: {r['default']} is red at {sha[:7]}; a Claude job {jid} fixes it")
    st["red"] = {"job": jid, "jobs": red.get("jobs", 0) + 1, "at": sha}
    save(key, st)


def hook(repo, st, due):
    """The repo's webhook to burrow-events (../events.nix): what happens on
    the repo starts its handlers' passes within seconds. Set again when its
    URL, events or secret change, and with the daily settings, in case it was
    deleted or edited. GitHub never gives the secret back, so it's known by a
    hash. The hook is the one with the id it was given, so a new URL moves
    it rather than leaving the old one sending. A status run has neither the secret (only the services get it)
    nor anything to change, so it leaves the hook alone."""
    if DRY:
        return
    wh = CFG["webhook"]
    secret = C.webhook_secret()
    want = hashlib.sha256(json.dumps([wh["url"], wh["events"], secret]).encode()).hexdigest()[:16]
    if st.get("hook") == want and not due:
        return
    hooks = C.gh("api", f"repos/{repo}/hooks", check=False)
    if not isinstance(hooks, list):
        return  # tried again on the next full pass
    body = json.dumps({"active": True, "events": wh["events"],
                       "config": {"url": wh["url"], "content_type": "json", "secret": secret, "insecure_ssl": "0"}})
    mine = ([x for x in hooks if x.get("id") == st.get("hookId")]
            or [x for x in hooks if (x.get("config") or {}).get("url") == wh["url"]])
    if mine:
        done = C.write("api", "-X", "PATCH", f"repos/{repo}/hooks/{mine[0]['id']}", "--input", "-", input=body, check=False)
        made = mine[0]["id"]
    else:
        log(f"{repo}: adding the webhook")
        done = C.write("api", "-X", "POST", f"repos/{repo}/hooks", "--input", "-",
                       input=json.dumps({"name": "web", **json.loads(body)}), check=False)
        made = done.get("id") if isinstance(done, dict) else None
    if done is not None:
        st["hook"], st["hookId"] = want, made
        save(f"repo-{repo.replace('/', '_')}.json", st)


def hook_projects(repos):
    """The hook on the projects' repos (projectsDir) that it doesn't see to
    (excluded, or another owner's): proj-watch and proj-inbox start from its
    deliveries too. Returns them, for unhook_gone to keep."""
    if DRY:
        return []
    seen = {r["repo"].lower() for r in repos}
    more = sorted({repo for _, repo in C.projects() if repo.lower() not in seen})
    for repo in more:
        name = f"repo-{repo.replace('/', '_')}.json"
        st = load(name, {})
        due = C.NOW - st.get("hookAt", 0) >= 86400
        if due:
            st["hookAt"] = C.NOW
            save(name, st)
        hook(repo, st, due)
    return [{"repo": repo} for repo in more]


def unhook_gone(repos):
    """The hooks this machine gave repos it no longer sees to (excluded,
    archived, out of the owners), off them: their deliveries would start
    passes over nothing, and the URL has no business there. One that can't
    be taken off (no admin any more, say), or whose name GitHub won't look
up, is tried again a day later. A
    renamed repo leaves its old state file behind, and GitHub answers the old
    name with the new one: that hook is the one hook() adopted under the new
    name, so the old file lets go of it instead."""
    if DRY:
        return
    live = {r["repo"].lower() for r in repos}
    keep = {f"repo-{r['repo'].replace('/', '_')}.json" for r in repos}
    for name in sorted(os.listdir(C.STATE)):
        if not (name.startswith("repo-") and name.endswith(".json")) or name in keep:
            continue
        st = load(name, {})
        if not st.get("hookId") or C.NOW - st.get("unhookTried", 0) < 86400:
            continue
        repo = name[len("repo-"):-len(".json")].replace("_", "/", 1)  # an owner has no _
        now = C.gh("api", f"repos/{repo}", check=False)
        if not isinstance(now, dict):  # not known to be gone: the old path may lead to a live repo
            st["unhookTried"] = C.NOW
            save(name, st)
            continue
        if str(now.get("full_name", "")).lower() in live:
            log(f"{repo}: renamed to {now['full_name']}, which keeps the webhook")
        else:
            log(f"{repo}: no longer seen to, taking its webhook off")
            if C.write("api", "-X", "DELETE", f"repos/{repo}/hooks/{st['hookId']}", check=False) is None:
                st["unhookTried"] = C.NOW
                save(name, st)
                continue
        for k in ("hook", "hookId", "unhookTried"):
            st.pop(k, None)
        save(name, st)


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


def sweep_branches(r, open_heads, digest, sweep=True):
    """Delete the merged branches and list the stale ones, once a day and
    only when `sweep`; the stale ones the last sweep found are listed every
    time."""
    repo = r["repo"]
    st = load(f"repo-{repo.replace('/', '_')}.json", {})
    if not sweep or C.NOW - st.get("branches", 0) < 86400:
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
