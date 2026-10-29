"""The repos: finding them, keeping their settings at the baseline, the
daily branch sweep, and the issues that list what waits for the owner and
what the pipeline is stuck on."""

import fnmatch
import hashlib
import json
import os
import subprocess
import tempfile
import urllib.parse

import common as C
from common import CFG, CONTEXT, DEP_RE, DRY, load, log, repo_cfg, save, ts
from tasks import RED_BRANCH, red_default_task


def discover():
    """Each owner's repos the App's installation there reaches: which ones
    are the steward's is which it's installed on. GitHub never reports an
    App as a repo's admin (an installation's viewerPermission is empty), so
    that's no test of what it may touch."""
    seen = []
    for owner in CFG["owners"]:
        with C.acting_for(owner):
            found = C.gh("repo", "list", owner, "--no-archived", "--source", "--limit", "100000",
                         "--json", "nameWithOwner,isPrivate,defaultBranchRef") or []
        for r in found:
            name = r["nameWithOwner"]
            if any(fnmatch.fnmatch(name, pat) for pat in CFG["exclude"]):
                continue
            # An empty repo has no branch to keep.
            if not r.get("defaultBranchRef"):
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
                # Only the App's own status: one with the same name from
                # anyone else (the owner's login included) satisfies nothing.
                "required_status_checks": [{"context": CONTEXT, "integration_id": CFG["app"]["id"]}],
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
    # Due daily, and straight away for another App: its required checks are
    # pinned to the one it was.
    due = C.NOW - st.get("settings", 0) >= 86400 or st.get("app") != CFG["app"]["id"]
    # The labels the steward and its jobs put on PRs: without them a hold
    # can't be set and a judge can't hand a PR to the owner. Made
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

    # Every page, an org's rulesets included. Only the repo's own baseline
    # counts as its baseline: the steward can't pin what the org keeps, so a
    # repo with the org's gets its own over it, and GitHub enforces both.
    rules = False
    try:
        pages = C.gh("api", f"repos/{repo}/rulesets?per_page=100", "--paginate", "--slurp")
    except RuntimeError as e:
        # The plan offers no rules on this repo (private, on a free
        # account): there are none to pin. Anything else is GitHub not
        # saying, and nothing's known to be pinned.
        pages, rules = None, "Upgrade to GitHub Pro" in str(e)
    if isinstance(pages, list):
        own = [x for page in pages for x in page if x.get("source_type") == "Repository"]
        made = True
        if CFG["rulesets"] and rc["ruleset"] and not any(x["name"] == "baseline" for x in own):
            log(f"{repo}: adding the baseline ruleset")
            made = C.write("api", "-X", "POST", f"repos/{repo}/rulesets", "--input", "-",
                           input=json.dumps(baseline_ruleset(rc, r["default"])), check=False) is not None
        # Whether or not the repo gets the baseline ruleset, its own rules
        # are kept in line with it.
        rules = keep_rules(repo, [x for x in own if x.get("target") == "branch"]) and made
    # Settled for this App only once every rule is known to take its status
    # from it alone; once that's not known, the settings are due again.
    pinned = keep_protection(repo) and rules

    st["settings"] = C.NOW
    if pinned:
        st["app"] = CFG["app"]["id"]
    else:
        st.pop("app", None)
    save(f"repo-{repo.replace('/', '_')}.json", st)
    return new


def keep_checks(checks, key):
    """The steward's own check among a rule's required ones, pinned to its
    App (under key: a ruleset's integration_id, classic protection's
    app_id): the same name from anyone else satisfies nothing. Returns
    whether any changed."""
    moved = False
    for c in checks:
        if c.get("context") == CONTEXT and c.get(key) != CFG["app"]["id"]:
            c[key] = CFG["app"]["id"]
            moved = True
    return moved


def keep_rules(repo, rulesets):
    """Each of the repo's own branch rulesets, in line with the baseline: no
    up-to-date branches (docs/repo-baseline.md says why), and the steward's
    status required from its App only. The listing names a ruleset without
    its rules, so each one is read. Returns False when one couldn't be read
    or wouldn't take the change."""
    ok = True
    for x in rulesets:
        rs = C.gh("api", f"repos/{repo}/rulesets/{x['id']}", check=False)
        if not isinstance(rs, dict):
            ok = False
            continue
        changed = []
        for y in rs.get("rules") or []:
            if y.get("type") != "required_status_checks":
                continue
            params = y.get("parameters") or {}
            if params.get("strict_required_status_checks_policy"):
                params["strict_required_status_checks_policy"] = False
                changed.append("up-to-date branches off")
            if keep_checks(params.get("required_status_checks") or [], "integration_id"):
                changed.append(f"{CONTEXT} from the App only")
        if changed:
            log(f"{repo}: {', '.join(dict.fromkeys(changed))} in the {rs.get('name')!r} ruleset")
            # The whole ruleset goes back, so only those settings change.
            keep = {k: rs[k] for k in ("name", "target", "enforcement", "bypass_actors", "conditions") if k in rs}
            put = C.write("api", "-X", "PUT", f"repos/{repo}/rulesets/{x['id']}", "--input", "-",
                          input=json.dumps({**keep, "rules": rs["rules"]}), check=False)
            ok = ok and put is not None
    return ok


def keep_protection(repo):
    """The same in the classic protection of every protected branch, since a
    PR can be merged into any of them. Returns False when GitHub didn't say
    what a branch's is, or didn't take the change."""
    pages = C.gh("api", f"repos/{repo}/branches?protected=true&per_page=100", "--paginate", "--slurp", check=False)
    if not isinstance(pages, list):
        return False
    return all([keep_branch(repo, b["name"]) for page in pages for b in page])


def keep_branch(repo, name):
    """One branch's classic protection, in line. Its checks are read only
    when the branch says it requires some: the endpoint is a 404 otherwise.
    Returns False when GitHub didn't say what they are, or didn't take the
    change."""
    # A slash in the name (release/1.0) is part of it, not of the path.
    branch = urllib.parse.quote(name, safe="")
    b = C.gh("api", f"repos/{repo}/branches/{branch}", check=False)
    if not isinstance(b, dict):
        return False
    checks = (b.get("protection") or {}).get("required_status_checks") or {}
    if checks.get("enforcement_level", "off") == "off":
        return True
    path = f"repos/{repo}/branches/{branch}/protection/required_status_checks"
    got = C.gh("api", path, check=False)
    if not isinstance(got, dict):
        return False
    patch, changed = {}, []
    if got.get("strict"):
        patch["strict"] = False
        changed.append("up-to-date branches off")
    listed = got.get("checks") or []
    if keep_checks(listed, "app_id"):
        # The others as they were: GitHub lists one any App may set with no
        # app_id, and reads one sent without it as "whichever set it last".
        patch["checks"] = [c if c.get("app_id") is not None else {**c, "app_id": -1} for c in listed]
        changed.append(f"{CONTEXT} from the App only")
    if not patch:
        return True
    log(f"{repo}: {', '.join(changed)} in {name}'s protection")
    return C.write("api", "-X", "PATCH", path, "--input", "-", input=json.dumps(patch), check=False) is not None


def watch_default(r, info, prs, digest):
    """A red default branch: CI failed on what's merged, and every PR is
    built on it. With no merge queue (docs/repo-baseline.md), this is how
    two PRs that broke it together get undone: a Claude job finds the
    cause and opens a PR from RED_BRANCH that reverts or fixes it, which
    lands like any other. Nothing more while that job works or its PR is
    open; a second job if it's still red after (seen on two passes), then
    the owner. Returns the PRs this pass mustn't merge: a fix PR made
    obsolete by the branch going green some other way."""
    repo = r["repo"]
    target = (info.get("defaultBranchRef") or {}).get("target") or {}
    sha, state = target.get("oid"), (target.get("statusCheckRollup") or {}).get("state")
    key = f"repo-{repo.replace('/', '_')}.json"
    st = load(key, {})
    red = st.get("red") or {}
    working = red.get("job") and C.job_state(red["job"]) in (None, "queued", "running")
    # Its PR: the one its task names, from the steward's own authors to this
    # branch. Anyone can open a PR from a branch of that name on a fork, and
    # that one is nothing of the steward's.
    fix = RED_BRANCH + (red.get("at") or "")[:12]
    open_fix = [pr for pr in prs if red.get("at") and pr["headRefName"] == fix
                and pr["baseRefName"] == r["default"] and C.acted_on(pr)]
    if state not in ("FAILURE", "ERROR"):
        if not red or state != "SUCCESS":
            return
        # Green while its job still works: wait for the job to end, so a PR
        # it opens can't outlive the red it was for, and leave alone one it
        # has opened. Once it has ended, its PR, if one is open, isn't needed.
        if working:
            return {pr["number"] for pr in open_fix}
        # Ended with no PR to show: as when red, seen on two passes before
        # the red is forgotten, since this one may have read the PRs just
        # before the job opened its own.
        if not open_fix and red.get("job") and not red.get("ended"):
            red["ended"] = True
            save(key, st)
            C.wake(repo, C.NOW + 60)  # ends: the second look, once
            return set()
        unclosed = []
        for pr in open_fix:
            log(f"{repo}: closing #{pr['number']}, {r['default']} is green again without it")
            if C.write("pr", "close", str(pr["number"]), "-R", repo, "--comment",
                       f"{r['default']} is green again at {sha[:7]}, so this fix for {red['at'][:7]} isn't needed.",
                       check=False) is None:
                unclosed.append(pr)
        # One GitHub wouldn't close is still known by the red it was for, and
        # tried again soon.
        if unclosed:
            C.wake(repo, C.NOW + 5 * 60)  # ends: the fix PR's own wait is overdue at its deadline (waits.py)
        else:
            log(f"{repo}: {r['default']} is green again")
            st.pop("red")
            save(key, st)
        return {pr["number"] for pr in open_fix}
    line = f"{repo} {r['default']} at {sha[:7]}"
    if working or open_fix:
        return
    # Its job ended with no PR to show. Seen on two passes: this one may
    # have read the PRs just before the job opened its own.
    if red.get("job") and not red.get("ended"):
        red["ended"] = True
        save(key, st)
        C.wake(repo, C.NOW + 60)  # ends: the second look, once
        return
    if red.get("jobs", 0) >= 2:
        digest.append(("The default branch is red", line, "two Claude jobs ended with it still red"))
        return
    jid = C.job(repo, red_default_task(repo, r["default"], sha))
    if not jid:
        digest.append(("Couldn't start a Claude job to fix it", line, "the default branch is red"))
        C.wake(repo, C.NOW + 5 * 60)  # ends: listed on the stuck issue
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
    deliveries too. Another owner's gets one only if the App is installed
    there too; one it can't reach is tried again a day later, and the rest
    still get theirs. Returns them all, for unhook_gone to keep: a hook
    whose repo couldn't be asked about stays on."""
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
        try:
            with C.acting_for(repo):
                hook(repo, st, due)
        except RuntimeError as e:  # the App isn't on its owner, or GitHub didn't answer
            log(f"{repo}: its webhook: {e}")
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
        try:
            with C.acting_for(repo):
                now = C.gh("api", f"repos/{repo}", check=False)
                gone = isinstance(now, dict) and str(now.get("full_name", "")).lower() not in live
                if gone:
                    log(f"{repo}: no longer seen to, taking its webhook off")
                off = gone and C.write("api", "-X", "DELETE", f"repos/{repo}/hooks/{st['hookId']}", check=False) is not None
        except RuntimeError as e:  # the App isn't on its owner (any more), say
            log(f"{repo}: {e}")
            now = off = None
        if not isinstance(now, dict) or (gone and not off):
            # Not known to be gone (the old path may lead to a live repo), or
            # its hook wouldn't come off.
            st["unhookTried"] = C.NOW
            save(name, st)
            continue
        if not gone:
            log(f"{repo}: renamed to {now['full_name']}, which keeps the webhook")
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
        p = subprocess.run(["git", "-C", d, *C.GIT_AUTH, "push", "-q", f"--force-with-lease=refs/heads/{name}:{sha}",
                            f"https://github.com/{repo}.git", f":refs/heads/{name}"],
                           capture_output=True, text=True, env=C.gh_env(repo))
        if p.returncode != 0:
            log(f"{repo}: kept {name}: {p.stderr.strip()[:200]}")


def closed_at(repo, name, sha):
    """The closed PR whose head was this branch at `sha`, if there is one.
    GitHub keeps that PR's commits (refs/pull/N/head) and offers to restore
    the branch from its page, so the branch holds nothing the PR doesn't."""
    owner = repo.split("/")[0]
    # Every page: a branch name reused for many PRs can have its match
    # past the first hundred.
    got = C.gh("api", "--paginate", f"repos/{repo}/pulls?state=closed&per_page=100&head={owner}:{urllib.parse.quote(name)}",
               "--jq", f'.[] | select(.head.sha == "{sha}") | .number', check=False)
    if got is None:
        return None
    first = str(got).split("\n")[0]
    return int(first) if first.isdigit() else None


def sweep_branches(r, in_use, digest, sweep=True):
    """Delete the merged branches and the closed PRs' branches, and list the
    stale ones, once a day and only when `sweep`; the stale ones the last
    sweep found are listed every time."""
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
        if not name or not sha or name == r["default"] or name in in_use:
            continue
        if any(fnmatch.fnmatch(name, pat) for pat in keep):
            continue
        # GitHub deletes a branch when its PR merges, never when it closes.
        # Matched by the tip, so a bot's branch pushed anew since (its next
        # update, with no PR yet) or anyone's pushed after the close stays.
        if (n := closed_at(repo, name, sha)) is not None:
            log(f"{repo}: deleting {name} at {sha[:7]} (#{n} was closed at it, and keeps its commits)")
            delete_branch(repo, name, sha)
            continue
        # Any other branch of a dependency bot's is its work in progress.
        if DEP_RE.match(name):
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


# What only the owner can settle: their call (a label, a judge's or an
# outside contribution's), a login, a branch nobody has decided on. The rest
# of the digest is the pipeline's own trouble (a job that keeps failing, a
# review that won't run, a merge GitHub won't take, a PR stuck a day), which
# a Claude job on this machine takes when proj's inbox is on.
OWNERS = ("Judged yours to decide", "Outside contributions", "Too much to read", "A reviewer is out of service",
          "No reviewer can review it", "Stale branches (unmerged, no PR, 30+ days)")


def publish(digest):
    """What waits for the owner, in one issue in config.notify.repo assigned
    to them; what the pipeline is stuck on, in another the inbox hands to a
    Claude job. All of it is the owner's when the inbox can't take it."""
    owners = (*OWNERS, f"Labelled {CFG['labels']['owner']}")
    off = inbox_off()
    owner = [x for x in digest if x[0] in owners or off]
    if CFG["inbox"]["enable"] and off and len(owner) > len([x for x in digest if x[0] in owners]):
        owner.append(("What's stuck is listed here: proj's inbox can't take it", off, ""))
    # No notify.repo: the digest goes to the journal, asking GitHub nothing.
    with C.acting_for(CFG["notify"]["repo"] or ""):
        issue("digest.json", "[autoland] PRs waiting for you", CFG["notify"]["label"], CFG["notify"]["assignee"], "",
              owner, "Nothing waiting.")
        # Closed once the inbox can't take it: what's in it is the owner's then.
        issue("stuck.json", "[autoland] Stuck in the pipeline", CFG["inbox"]["label"], "", stuck_brief(),
              [x for x in digest if x not in owner], "Nothing stuck.")


def inbox_off():
    """Why proj's inbox won't take the stuck issue, or "" when it will: it
    looks only at repos cloned in projectsDir, and only at issues one of its
    authors opened, and the steward opens this one as its own login. Asked
    each pass, so it follows a clone or a login that goes away."""
    ib, nrepo = CFG["inbox"], CFG["notify"]["repo"]
    if not ib["enable"]:
        return "it's off"
    if not nrepo:  # the digest goes to the journal: there's no issue to take
        return "notify.repo isn't set"
    if not C.project_dir(nrepo):
        return f"{nrepo} isn't cloned in {CFG['projectsDir']}"
    if C.me() not in ib["authors"]:
        return f"the steward's login, {C.me()}, isn't one of its authors"
    return ""


def stuck_brief():
    who = CFG["notify"]["assignee"] or "the owner"
    stalls = " (and `burrow-watchdog status` for what stalled past its deadline)" if CFG["watch"] else ""
    return f"""The PR pipeline on this machine is stuck on what's below, and none of it is {who}'s to settle. For each \
one, find out why: `journalctl -u autoland` and `-u autoland-review` here, and the steward's state in \
{CFG['stateDir']}{stalls}. Fix the cause: a burrow PR with a test that fails on the bug when it's burrow's code, the fix on \
this machine when it's the machine. Then get it moving again (`autoland-steward kick OWNER/REPO` starts a repo's \
pass). Your answer, posted here, says what you found and did for each. The steward keeps this issue up to date: \
something new stuck comes as a comment, and it closes the issue itself once nothing is, so don't close it. If one \
really does need {who} (a login only they can do, a call that's theirs), say exactly what and assign this issue to \
them.

"""


def issue(key, title, label, assignee, intro, digest, done):
    """One issue in config.notify.repo listing digest, kept up to date: each
    new or changed entry is a comment too, and it's closed once there's none."""
    nrepo = CFG["notify"]["repo"]
    groups = {}
    for why, line, desc in digest:
        groups.setdefault(why, []).append(f"- {line}" + (f": {desc}" if desc else ""))
    body = "\n\n".join(f"**{why}**\n" + "\n".join(lines) for why, lines in groups.items())
    # Whole entries, so a PR listed again for something else, or for the
    # same thing with another diagnosis, is news too.
    entries = sorted({f"{why}: {line}" + (f": {desc}" if desc else "") for why, line, desc in digest})
    if DRY or not nrepo:
        log(f"--- {title} ---\n" + (body or "(nothing)"))
        return
    prev = load(key, {"entries": []})
    # Unchanged since the last look, under an hour ago: nothing to read or write.
    if prev.get("body") == body and C.NOW - prev.get("checked", 0) < 3600:
        return
    found = C.gh("issue", "list", "-R", nrepo, "--label", label, "--state", "open", "--search",
                 f'"{title}" in:title', "--json", "number,body", check=False)
    if not isinstance(found, list):
        return  # couldn't look (no quota?): try next pass, and don't open a second issue
    found = [i for i in found if i]
    # Recorded as published only once every call went through, so a failed
    # one is tried again next pass rather than an hour later.
    ok = True
    if not digest:
        if found:
            ok = C.gh("issue", "close", str(found[0]["number"]), "-R", nrepo, "--comment", done, check=False) is not None
        if ok:
            save(key, {"entries": [], "body": body, "checked": C.NOW})
        return
    full = intro + body + "\n\n_Kept up to date by autoland-steward on " + os.uname().nodename + "._"
    if not found:
        C.gh("label", "create", label, "-R", nrepo, "--color", "B60205", check=False)
        args = ["issue", "create", "-R", nrepo, "--label", label, "--title", title, "--body", full]
        if assignee:
            args += ["--assignee", assignee]
        ok = C.gh(*args, check=False) is not None
    else:
        num = str(found[0]["number"])
        if found[0]["body"] != full:
            ok = C.gh("issue", "edit", num, "-R", nrepo, "--body", full, check=False) is not None
        added = [x for x in entries if x not in prev.get("entries", [])]
        if added and ok:
            ok = C.gh("issue", "comment", num, "-R", nrepo, "--body", "New:\n" + "\n".join(f"- {x}" for x in added), check=False) is not None
    if ok:
        save(key, {"entries": entries, "body": body, "checked": C.NOW})
