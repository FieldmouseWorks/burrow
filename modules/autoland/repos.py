"""The repos: finding them, keeping their settings at the baseline, deleting
the branches nothing needs, and the issues that list what waits for the
owner and what the pipeline is stuck on."""

import fnmatch
import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
import time
import urllib.parse

import common as C
import reviewers as V
import train as T
from common import CFG, CONTEXT, DEP_RE, DRY, load, log, repo_cfg, save, ts
from prs import PROOF_SLOT, RELEASE_AGE, REVIEWER
from tasks import RED_BRANCH, red_default_task

# What a project's state says when GitHub had no such repo at its last look.
GONE = "gone"


# Each owner's own repos, not forks, a page at a time (what `gh repo list
# --source` asks, with the repo's databaseId, which it can't give).
REPOS = """query($owner: String!, $cursor: String) {
  repositoryOwner(login: $owner) {
    repositories(first: 100, after: $cursor, isFork: false, ownerAffiliations: OWNER) {
      nodes { databaseId nameWithOwner isPrivate isArchived createdAt defaultBranchRef { name } }
      pageInfo { hasNextPage endCursor }
    }
  }
}"""


def discover():
    """Each owner's repos the App's installation there reaches: which ones
    are the steward's is which it's installed on. GitHub never reports an
    App as a repo's admin (an installation's viewerPermission is empty), so
    that's no test of what it may touch. Each is known by its databaseId,
    the number GitHub keeps for the repo whatever it's called (its node id
    is no such thing: GitHub has changed their format before)."""
    seen = []
    for owner in CFG["owners"]:
        cursor, found = None, []
        with C.acting_for(owner):
            while True:
                got = (C.graphql(REPOS, owner=owner, cursor=cursor)["repositoryOwner"] or {}).get("repositories")
                if not got:
                    break
                found += got["nodes"]
                if not got["pageInfo"]["hasNextPage"]:
                    break
                cursor = got["pageInfo"]["endCursor"]
        for r in found:
            name = r["nameWithOwner"]
            if r["isArchived"] or any(fnmatch.fnmatch(name, pat) for pat in CFG["exclude"]):
                continue
            # An empty repo has no default branch yet (None): only its
            # settings and hook, until its first push.
            seen.append({"repo": name, "id": r["databaseId"], "private": r["isPrivate"],
                         "default": (r.get("defaultBranchRef") or {}).get("name"), "created": r["createdAt"]})
    return seen


def required(train):
    """The checks the baseline requires: the App's own status, and in a repo
    with its train on the train's, which it posts just before it lands a
    PR. Only the App's: one with the same name from anyone else (the
    owner's login included) satisfies nothing."""
    return [{"context": c, "integration_id": CFG["app"]["id"]} for c in [CONTEXT] + [T.CONTEXT] * bool(train)]


def bypass_actors(rc):
    """Who gets past the baseline. The owner (an admin) can still merge past
    it from the PR page, and automation that pushes (directPush) past it
    altogether; in a repo with its train on, no one: only the train lands
    there."""
    if rc["train"]:
        return []
    return [{"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always" if rc["directPush"] else "pull_request"}]


def baseline_ruleset(rc):
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
                "required_status_checks": required(rc["train"]),
            }},
        ]
    return {
        "name": "baseline",
        "target": "branch",
        "enforcement": "active",
        "conditions": {"ref_name": {"include": ["~DEFAULT_BRANCH"], "exclude": []}},
        "bypass_actors": bypass_actors(rc),
        "rules": rules,
    }


# What each of the steward's waiting labels (prs.py) says it waits on, and
# what the PR's status adds.
WAITING = {RELEASE_AGE: f"Its updates are too new to take yet (the minimum release age); {CONTEXT} says until when",
           PROOF_SLOT: f"Waits its turn for a proof slot on the machine; {CONTEXT} says how many are ahead",
           REVIEWER: f"Waits for a reviewer to be back in service; {CONTEXT} says which and why"}


def settle_repo(r, known):
    """The repo's settings, at most once a day. Returns True if it's new:
    not in known (restart takes a remade one out)."""
    repo, rc = r["repo"], repo_cfg(r["repo"])
    st = load(f"repo-{repo.replace('/', '_')}.json", {})
    new = repo not in known
    # GitHub's databaseId for it, by which restart tells a remade repo: kept
    # once restart has passed it, settings kept before it was included. The
    # node id kept before it goes: it's no test of which repo this is.
    if r.get("id") and (st.get("databaseId") != r["id"] or "id" in st):
        st.pop("id", None)
        st["databaseId"] = r["id"]
        save(f"repo-{repo.replace('/', '_')}.json", st)
    # Due daily, and straight away for another App (its required checks are
    # pinned to the one it was), a train turned on or off (the baseline
    # requires the train's status only while it's on), or a repo settled
    # empty that has its first commit now (it's due its ruleset).
    empty = r["default"] is None
    due = (C.NOW - st.get("settings", 0) >= 86400 or st.get("app") != CFG["app"]["id"]
           or st.get("train", False) != rc["train"] or (st.get("empty", False) and not empty))
    # The labels the steward and its jobs put on PRs: without them a hold
    # can't be set, a judge can't hand a PR to the owner, no PR can go first
    # in its proof line and none can say what it waits on. Made again with
    # the daily settings too, in case one was deleted.
    labels = CFG["labels"]
    about = [(labels["hold"], "fbca04", "The steward leaves this PR alone"),
             (labels["owner"], "b60205", f"Waits for {CFG['owner']}; take the label off to let the steward carry on"),
             (labels["first"], "0e8a16", "Goes ahead of the PRs without it in the line for proofs that take the machine"),
             *((labels["waiting"][kind], "c5def5", text) for kind, text in WAITING.items())]
    want = [name for name, _, _ in about]
    if st.get("labels") != want or due:
        # GitHub takes a description of up to 100 characters.
        made = [C.write("label", "create", name, "-R", repo, "--force", "--color", color,
                        "--description", text[:100], check=False) is not None
                for name, color, text in about]
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
        pages, rules = None, C.NO_RULES in str(e)
    if isinstance(pages, list):
        own = [x for page in pages for x in page if x.get("source_type") == "Repository"]
        made = True
        # Not on a repo with no commit yet: the ruleset's checks and PR rule
        # would turn away the push that makes its default branch, the
        # owner's too (they bypass it only by PR). That push's pass adds it.
        if CFG["rulesets"] and rc["ruleset"] and not empty and not any(x["name"] == "baseline" for x in own):
            log(f"{repo}: adding the baseline ruleset")
            made = C.write("api", "-X", "POST", f"repos/{repo}/rulesets", "--input", "-",
                           input=json.dumps(baseline_ruleset(rc)), check=False) is not None
        # Whether or not the repo gets the baseline ruleset, its own rules
        # are kept in line with it.
        rules = keep_rules(repo, rc, [x for x in own if x.get("target") == "branch"]) and made
    # Settled for this App only once every rule is known to take its status
    # from it alone; once that's not known, the settings are due again.
    pinned = keep_protection(repo) and rules

    st["settings"] = C.NOW
    if empty:
        st["empty"] = True
    else:
        st.pop("empty", None)
    if pinned:
        st["app"] = CFG["app"]["id"]
        st["train"] = rc["train"]
    else:
        st.pop("app", None)
    save(f"repo-{repo.replace('/', '_')}.json", st)
    return new


def restart(r, known, failed):
    """A repo made under the name of one GitHub deleted (GitHub hands a
    deleted repo's name to the next one) starts over as new, before
    anything acts on it, when the settings the steward last set under its
    name were another repo's (C.replaced). Then `reset`."""
    repo = r["repo"]
    for k, st in C.kept_state(repo):
        if C.replaced(st, r):
            log(f"{repo}: made at {r['created']}, and isn't the repo of its name the steward settled; starting it over as new")
            if not DRY:
                reset(r, k, known, failed)


def reset(r, k, known, failed):
    """The deleted repo whose state is under k goes: its clones are moved
    aside, proj-inbox forgets its issues and its Claude jobs are stopped
    (they'd push its history to the new one, or answer the new one's
    issues), and all the steward kept about it goes: its PRs' state (so a new
    PR doesn't take on an old one's of the same number), their reviews,
    snapshots and the reviewers' sessions (so no look at a new PR resumes
    an old one's), its dashboard's and quarantine's, and the reviewers'
    clone of it. The name is out of known, and of failed, so the new one's
    baseline job starts straight away.

    Its settings (repo-K.json) are what says it was remade, so they go
    last: a pass that fails partway leaves them, and the next one does it
    all again, each step as safe twice as once. Raises when it can't be
    done, and then nothing may act on the repo this pass."""
    repo = r["repo"]
    for x in [x for x in known if x.lower() == repo.lower()]:
        known.discard(x)
    for x in [x for x in failed if x.lower() == repo.lower()]:
        failed.pop(x)
    save("known.json", sorted(known))
    save("baseline-failed.json", failed)
    retire_checkouts(repo, k)
    left = V.sweep({k}, set())
    if left:
        raise RuntimeError(f"its reviewers aren't done with {', '.join(left)}: a look under way, or a session "
                           "that couldn't be marked gone")
    mirrors = os.path.join(C.STATE, V.GIT)
    for x in sorted(os.listdir(mirrors)) if os.path.isdir(mirrors) else []:
        if x.lower() == f"{repo.replace('/', '__')}.git".lower():
            shutil.rmtree(os.path.join(mirrors, x))
    mine = re.compile(rf"(dashboard|flaky)-{re.escape(k)}\.json|pr-{re.escape(k)}-\d+\.json")
    for name in sorted(os.listdir(C.STATE)):
        if mine.fullmatch(name):
            os.remove(os.path.join(C.STATE, name))
    os.remove(os.path.join(C.STATE, f"repo-{k}.json"))


def retire_checkouts(repo, k):
    """Every clone of the repo in projectsDir moved aside under a hidden
    name, which nothing takes for a project: so the repo's next job clones
    it afresh, and none can push the deleted one's history to it. Nothing
    in one is lost. Which repo a clone is of can't be told: all it knows is
    the name it came from, which may have been the deleted one's then,
    however long ago the repo there now was made (one renamed into the name
    may be years older). So the new one's own goes too, and its next job
    clones it again.

    Then proj-inbox forgets the repo's issues (C.forget_issues), so the new
    one's issue N is a new job, never the deleted one's N, and no answer
    of an old job's goes to it. Then every Claude job of the repo's is
    stopped, from proj's own record of every job (C.jobs): each the repo's
    by the repo its kicks name (the steward's jobs and proj-inbox's, in
    whatever folder) or by its folder (one moved aside, or the folder the
    repo's next clone takes, unless another repo's clone is there). A job
    whose kicks name only other repos is theirs, wherever it ran. The moves
    go first, and then the inbox waits for a pass of its under way, so a
    job either starts before the jobs are read or can't start at all: none
    starts in a clone moved aside, and the inbox starts none for a repo it
    has no clone of. Each folder is kept with the repo's settings
    (`retired`) before it's moved, so a later try stops its jobs if this
    one can't. A job proj won't stop raises, for the next pass to try again."""
    src = CFG["projectsDir"]
    every = list(C.projects(strict=True))
    st = load(f"repo-{k}.json", {})
    retired = st.setdefault("retired", [])
    for d in sorted(d for d, url in every if url.lower() == repo.lower()):
        aside = os.path.join(src, f".{d}.gone-{time.strftime('%Y%m%dT%H%M%SZ', time.gmtime(C.NOW))}")
        log(f"{repo}: its checkout {d} may be the deleted one's; moving it to {aside}")
        if d not in retired:
            retired.append(d)
            save(f"repo-{k}.json", st)
        os.rename(os.path.join(src, d), aside)
    C.forget_issues(repo)
    ours = {os.path.join(src, d) for d in set(retired) | ({repo.split("/")[1]} - {d for d, _ in every})}
    for jid, project, repos in C.jobs():
        if (repo.lower() in repos or (not repos and project in ours)) and not C.stop_job(jid):
            raise RuntimeError(f"{repo}: proj couldn't stop job {jid}, which may be the deleted one's")


def keep_checks(checks, key):
    """The steward's own checks among a rule's required ones (its status,
    its train's), pinned to its App (under key: a ruleset's integration_id,
    classic protection's app_id): the same name from anyone else satisfies
    nothing. Returns whether any changed."""
    moved = False
    for c in checks:
        if c.get("context") in (CONTEXT, T.CONTEXT) and c.get(key) != CFG["app"]["id"]:
            c[key] = CFG["app"]["id"]
            moved = True
    return moved


def complete_baseline(rs, rc):
    """A baseline made before this repo's settings were what they are (say
    while it pushed directly, with only the first two rules) gets the rules
    it lacks: with no required-status rule in it there's nothing for
    keep_rules to require the train's status in. Who may bypass it follows
    the train going on or off. Returns what changed, in words."""
    if rs.get("name") != "baseline" or not (CFG["rulesets"] and rc["ruleset"]) or rc["directPush"]:
        return []
    changed = []
    have = {y.get("type") for y in rs.get("rules") or []}
    missing = [y for y in baseline_ruleset(rc)["rules"] if y["type"] not in have]
    if missing:
        rs["rules"] = (rs.get("rules") or []) + missing
        changed.append(f"{', '.join(y['type'] for y in missing)} added")
    # Only the train's own turn: a repo's other bypass is its own to keep.
    if bool(rs.get("bypass_actors")) == bool(rc["train"]):
        rs["bypass_actors"] = bypass_actors(rc)
        changed.append("no bypass, only the train lands" if rc["train"] else "the owner's bypass back")
    return changed


def keep_rules(repo, rc, rulesets):
    """Each of the repo's own branch rulesets, in line with the baseline: no
    up-to-date branches (docs/repo-baseline.md says why), and the steward's
    status required from its App only; the baseline's own has all its rules
    (complete_baseline) and requires the train's too exactly while the repo's
    train is on. The listing names a
    ruleset without its rules, so each one is read. Returns False when one
    couldn't be read or wouldn't take the change."""
    ok = True
    for x in rulesets:
        rs = C.gh("api", f"repos/{repo}/rulesets/{x['id']}", check=False)
        if not isinstance(rs, dict):
            ok = False
            continue
        changed = complete_baseline(rs, rc)
        for y in rs.get("rules") or []:
            if y.get("type") != "required_status_checks":
                continue
            params = y.get("parameters") or {}
            if params.get("strict_required_status_checks_policy"):
                params["strict_required_status_checks_policy"] = False
                changed.append("up-to-date branches off")
            if keep_checks(params.get("required_status_checks") or [], "integration_id"):
                changed.append("the steward's checks from the App only")
            listed = params.get("required_status_checks") or []
            has = any(c.get("context") == T.CONTEXT for c in listed)
            if rs.get("name") == "baseline" and has != bool(rc["train"]):
                params["required_status_checks"] = (listed + [{"context": T.CONTEXT, "integration_id": CFG["app"]["id"]}]
                                                    if rc["train"] else [c for c in listed if c.get("context") != T.CONTEXT])
                changed.append(f"{T.CONTEXT} {'required' if rc['train'] else 'no longer required'}")
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
        changed.append("the steward's checks from the App only")
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
    nor anything to change, so it leaves the hook alone. False when GitHub
    wouldn't give the hooks or take the change."""
    if DRY:
        return True
    wh = CFG["webhook"]
    secret = C.webhook_secret()
    want = hashlib.sha256(json.dumps([wh["url"], wh["events"], secret]).encode()).hexdigest()[:16]
    if st.get("hook") == want and not due:
        return True
    hooks = C.gh("api", f"repos/{repo}/hooks", check=False)
    if not isinstance(hooks, list):
        return False  # tried again on the next full pass
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
    if done is None:
        return False
    st["hook"], st["hookId"] = want, made
    save(f"repo-{repo.replace('/', '_')}.json", st)
    return True


def app_hook():
    """The App's own webhook, to burrow-events like each repo's, with the same
    secret: it's how a repo just made is heard (GitHub sends the App the
    `repository` events of every repo its installations reach, a new one's
    included, before the repo has a hook of its own). The App's settings
    turn it on and choose its events; its URL and secret are kept here, set
    again when they or the App change and once a day, in case they were
    edited. GitHub never gives the secret back, so it's known by a hash."""
    if DRY:
        return
    wh = CFG["webhook"]
    want = hashlib.sha256(json.dumps([CFG["app"]["id"], wh["url"], C.webhook_secret()]).encode()).hexdigest()[:16]
    st = load("app-hook.json", {})
    if st.get("hook") == want and C.NOW - st.get("at", 0) < 86400:
        return
    try:
        C.app_call("PATCH", "app/hook/config", C.app_jwt(),
                   {"url": wh["url"], "content_type": "json", "secret": C.webhook_secret(), "insecure_ssl": "0"})
    except RuntimeError as e:  # tried again on the next full pass
        log(f"the App's webhook: {e}")
        return
    log("the App's webhook: set")
    save("app-hook.json", {"hook": want, "at": C.NOW})


def hook_projects(repos):
    """The hook on the projects' repos (projectsDir) that it doesn't see to
    (excluded, or another owner's): proj-watch and proj-inbox start from its
    deliveries too. Another owner's gets one only if the App is installed
    there too, and a clone whose repo GitHub has no more (deleted, or out of
    the App's reach) gets none. Either is said once, then left alone until a
    day later, when it's tried again (a gone one by looking the repo up,
    quietly), and the rest still get theirs. GitHub answers a deleted repo
    and one the App lost access to alike, so the hook's record stays: back,
    the repo's hook is the one it had. Returns them all, for unhook_gone to
    keep: a hook whose repo couldn't be asked about stays on."""
    if DRY:
        return []
    seen = {r["repo"].lower() for r in repos}
    more = sorted({repo for _, repo in C.projects() if repo.lower() not in seen})
    for repo in more:
        name = f"repo-{repo.replace('/', '_')}.json"
        st = load(name, {})
        due = C.NOW - st.get("hookAt", 0) >= 86400
        was = st.get("unreached")
        if was and not due:
            continue
        if due:
            st["hookAt"] = C.NOW
            save(name, st)
        why = None
        try:
            with C.acting_for(repo):
                if was == GONE:
                    C.fresh(f"repos/{repo}")  # asking for its hooks would only fail again
                if not hook(repo, st, due):
                    # Its hooks couldn't be read or set: Gone if it's the
                    # repo itself that GitHub doesn't have.
                    C.fresh(f"repos/{repo}")
        except C.Gone:
            why = GONE
        except C.Unavailable as e:  # GitHub didn't answer: nothing new is known
            log(f"{repo}: its webhook: {e}")
            why = was
        except RuntimeError as e:  # the App isn't on its owner, or GitHub said no to the lookup
            why = str(e)
        if why == was:
            continue
        if why == GONE:
            log(f"{repo}: GitHub has no such repo, so its clone in {CFG['projectsDir']} gets no webhook; "
                "looked up again daily")
        elif why:
            log(f"{repo}: its webhook: {why}; tried again daily")
        else:
            log(f"{repo}: {'on GitHub' if was == GONE else 'reached'} again")
        if why:
            st["unreached"] = why
        else:
            st.pop("unreached", None)
        save(name, st)
    return [{"repo": repo} for repo in more]


def unhook_gone(repos):
    """The hooks this machine gave repos it no longer sees to (excluded,
    archived, out of the owners), off them: their deliveries would start
    passes over nothing, and the URL has no business there. One that can't
    be taken off (no admin any more, say), or whose name GitHub doesn't
    answer for, is tried again a day later. GitHub answers a deleted repo
    and one the App can't reach any more alike, so the hook stays on record
    and is looked up again a day later too, but said only once. A renamed
    repo leaves its old state file behind, and GitHub answers the old name
    with the new one: that hook is the one hook() adopted under the new
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
        now = off = None
        try:
            with C.acting_for(repo):
                now = C.fresh(f"repos/{repo}")[1]
                st.pop("unreached", None)
                gone = str(now.get("full_name", "")).lower() not in live
                if gone:
                    log(f"{repo}: no longer seen to, taking its webhook off")
                off = gone and C.write("api", "-X", "DELETE", f"repos/{repo}/hooks/{st['hookId']}", check=False) is not None
        except C.Gone:
            if st.get("unreached") != GONE:
                log(f"{repo}: GitHub has no such repo the App can see, so its webhook stays on record; looked up again daily")
            st["unreached"] = GONE
        except RuntimeError as e:  # the App isn't on its owner (any more), or GitHub didn't answer
            log(f"{repo}: {e}")
        if now is None or (gone and not off):
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


# Unmerged, with no PR and no commit for this long: given up on (agents open
# a PR when they push). Its commits are kept under ARCHIVE and the branch
# deleted, so a short wait costs nothing.
STALE_DAYS = 3
ARCHIVE = "refs/archive"


def answer(call, *args):
    """What GitHub answered call(*args) (C.gh or C.write), or None when it
    said no (a branch with no history in common with the default has no
    comparison, say). C.Unavailable when it didn't answer, which ends the
    pass's look at the branches: the next pass takes up the rest."""
    try:
        return call(*args)
    except C.Unavailable:
        raise
    except RuntimeError as e:
        log(f"  failed: {e}")
        return None


def archive_branch(repo, name, sha):
    """Keep the branch's commits at refs/archive/NAME/SHA, a ref GitHub keeps
    but lists with neither branches nor tags; True once they're kept (by
    this call or an earlier one), and only then may the branch go.
    C.Unavailable when GitHub didn't answer."""
    ref = f"{ARCHIVE}/{name}/{sha}"
    if DRY:
        log(f"  would: keep {repo} {name} at {ref}")
        return True
    if answer(C.write, "api", f"repos/{repo}/git/refs", "-f", f"ref={ref}", "-f", f"sha={sha}") is not None:
        return True
    # Already there: an earlier look kept it and then couldn't delete.
    return answer(C.gh, "api", f"repos/{repo}/git/ref/{urllib.parse.quote(ref.removeprefix('refs/'))}",
                  "--jq", f'.object.sha == "{sha}"') is True


def closed_at(repo, name, sha):
    """The closed PR whose head was this branch at `sha`, 0 when there's
    none. GitHub keeps that PR's commits (refs/pull/N/head) and offers to
    restore the branch from its page, so the branch holds nothing the PR
    doesn't. None when GitHub said no; C.Unavailable when it didn't answer."""
    owner = repo.split("/")[0]
    # Every page: a branch name reused for many PRs can have its match
    # past the first hundred.
    got = answer(C.gh, "api", "--paginate", f"repos/{repo}/pulls?state=closed&per_page=100&head={owner}:{urllib.parse.quote(name)}",
                 "--jq", f'.[] | select(.head.sha == "{sha}") | .number')
    if got is None:
        return None
    first = str(got).split("\n")[0]
    return int(first) if first.isdigit() else 0


def closed_heads(repo, since, etag):
    """What a PR closing since the last look (`since`, its newest change)
    may have left: (the etag to ask by next time, the newest change now,
    {(branch, tip)}), each the branch here a PR closed, or changed while
    closed, was from and the tip it closed at. None while nothing has, a
    look GitHub doesn't count against the rate limit. The heads are None
    when every branch is to be looked at again: there was no look before,
    or more has changed since than one page holds. RuntimeError when GitHub
    didn't list them."""
    got = C.fresh(f"repos/{repo}/pulls?state=closed&sort=updated&direction=desc&per_page=100", etag)
    if got is None:
        return None
    etag, page = got
    # The same second as the last look's newest again: one closed in it
    # may not have been listed yet.
    newer = [p for p in page if since is not None and p["updated_at"] >= since]
    heads = None if since is None or len(newer) == 100 else {
        (p["head"]["ref"], p["head"]["sha"]) for p in newer if (p["head"].get("repo") or {}).get("full_name") == repo}
    return etag, page[0]["updated_at"] if page else "", heads


UNKEPT = f"Stale branches it couldn't archive (unmerged, no PR, {STALE_DAYS}+ days)"


# A branch no open PR has goes only once it has sat at one tip this long, so
# a PR opened (or reopened) on it just after its PR closed, or the first
# commit on one just pushed at the default's tip, keeps it.
GRACE = 3600


def watch_branches(r, in_use, digest, full=False):
    """Delete the branches nothing needs any more, on what changes them: a
    push to one (the listing changes), a PR of its closing (it leaves
    `in_use`, the open PRs' branches, or is among the PRs closed since the
    last look), the default moving under it, or the wait it was given
    ending (wake.json). Every pass looks, at no cost to the rate limit while
    nothing changed. A full pass also tries again what GitHub said no to: a
    stale one's archive, or a read a judgment needed. A pass GitHub stopped
    answering keeps what it judged and leaves the rest to the next one."""
    repo = r["repo"]
    path = f"repo-{repo.replace('/', '_')}.json"
    st = load(path, {})
    tips = st.get("tips", {})
    # The machine's globs, plus the repo's own (evidence branches it cites),
    # and its train's candidates', which cn makes and deletes.
    keep = CFG["keepBranches"] + repo_cfg(repo).get("keepBranches", []) + [T.TRAIN.get("branches")] * T.on(repo)
    # The last listing stands for this one while it's unchanged, unless what
    # it's sorted by has changed since: a branch no longer kept, or no
    # longer the default, is judged.
    by = {"default": r["default"], "keep": keep}
    try:
        got = listed_branches(repo, r["default"], st.get("branchesEtag") if st.get("branchesBy") == by else None)
        listed, base = (got[1], got[2]) if got else ({name: tip["sha"] for name, tip in tips.items()}, st.get("base"))
        # Every comparison was against the default's tip: one that has moved
        # (a merge into it, a force-push) may judge a branch otherwise.
        moved = base != st.get("base")
        closed = closed_heads(repo, st.get("closedSince"), st.get("closedEtag"))
        heads = set() if closed is None else closed[2]
        for name in set(tips) - set(listed):
            del tips[name]  # deleted, here or by anyone
        for name, sha in listed.items():
            was = tips.get(name)
            if name == r["default"] or any(fnmatch.fnmatch(name, pat) for pat in keep):
                tips.pop(name, None)
            elif name in in_use:
                tips[name] = {"sha": sha, "pr": True}
            elif was is None or was["sha"] != sha or was.get("pr") or heads is None or (name, sha) in heads:
                # New, pushed to, or a PR of its closed since: the wait starts now.
                tips[name] = judge(repo, r, name, sha, C.NOW)
            elif moved or looks_again(was, full):
                tips[name] = judge(repo, r, name, sha, was["seen"])
        # Only once every branch is judged by them: a pass that stopped
        # partway leaves them for the next one to judge the rest by too.
        if got:
            st.update(branchesEtag=got[0], branchesBy=by, base=base)
        if closed:
            st.update(closedEtag=closed[0], closedSince=closed[1])
    except RuntimeError as e:
        log(f"{repo}: its branches, left for the next pass: {e}")
    for k in ("branches", "unkept", "stale"):
        st.pop(k, None)  # the daily sweep's, which nothing reads now
    st["tips"] = tips
    save(path, st)
    due = [tip["due"] for tip in tips.values() if tip.get("due", 0) > C.NOW and not tip.get("unkept")]
    if due:
        C.wake(repo, min(due))  # ends: that branch's look, when it goes or has a push or a PR since
    for name, tip in sorted(tips.items()):
        if tip.get("unkept"):
            digest.append((UNKEPT, f"{repo}: `{name}`", ""))


def looks_again(tip, full):
    """Whether a branch judged before, and unchanged since, is judged again
    this pass: its wait is over, or (a full pass) GitHub said no last time,
    to archiving it or to a read its judgment needed."""
    if tip.get("unkept") or tip.get("retry"):
        return full
    return tip.get("due", C.NOW + 1) <= C.NOW


def listed_branches(repo, default, etag):
    """(its etag, {name: tip} of repo's unprotected branches, [default, its
    tip]), or None when they're as `etag` saw them (GitHub doesn't count
    that look against the rate limit). RuntimeError when GitHub didn't list
    them."""
    got = C.fresh(f"repos/{repo}/branches?per_page=100", etag)
    if got is None:
        return None
    etag, page = got
    if len(page) >= 100:
        # More than a page: every page, and no etag to ask by next time.
        out = C.gh("api", "--paginate", f"repos/{repo}/branches?per_page=100", "--jq",
                   '.[] | "\\(.name) \\(.commit.sha) \\(.protected)"')
        page = [{"name": n, "commit": {"sha": sha}, "protected": p == "true"}
                for n, sha, p in (x.split(" ") for x in str(out).split("\n") if x.count(" ") == 2)]
        etag = None
    tips = {b["name"]: b["commit"]["sha"] for b in page}
    return etag, {b["name"]: b["commit"]["sha"] for b in page if not b.get("protected")}, [default, tips.get(default)]


def judge(repo, r, name, sha, seen):
    """A branch no open PR has, at `sha` since `seen`: its record, with when
    it's due a look again (none: only on a push, a PR or the default
    moving; or a full pass, when GitHub said no to a read it needed), and
    deleted once that's past and nothing needs it. C.Unavailable when
    GitHub didn't answer."""
    tip = {"sha": sha, "seen": seen}
    # GitHub deletes a branch when its PR merges, never when it closes.
    # Matched by the tip, so a bot's branch pushed anew since (its next
    # update, with no PR yet) or anyone's pushed after the close stays.
    n = closed_at(repo, name, sha)
    if n is None:
        return {**tip, "retry": True}
    if n:
        due, why = seen + GRACE, f"#{n} was closed at it, and keeps its commits"
    elif DEP_RE.match(name):
        return tip  # any other branch of a dependency bot's is its work in progress
    else:
        # Compared at the tip just listed, and deleted only if it's still there.
        cmp = answer(C.gh, "api", C.compare(repo, r["default"], sha), "--jq",
                     "{ahead: .ahead_by, date: .commits[-1].commit.committer.date}")
        if not isinstance(cmp, dict) or not (cmp["ahead"] == 0 or cmp.get("date")):
            # Nothing to judge it by. GitHub's no reads the same whether
            # it's for now (its access) or for good (no history in common
            # with the default), so it's asked again.
            return {**tip, "retry": True}
        if cmp["ahead"] == 0:
            due, why = seen + GRACE, f"already in {r['default']}"
        else:
            due, why = max(seen + GRACE, ts(cmp["date"]) + STALE_DAYS * 86400), None
    tip["due"] = due
    if C.NOW < due:
        return tip
    if why is None:
        # Unmerged, with no PR and no commit for STALE_DAYS. Nothing is
        # lost: one ref made from the archived one restores it.
        if not archive_branch(repo, name, sha):
            return {**tip, "unkept": True}
        why = f"unmerged, no PR, {STALE_DAYS}+ days; kept at {ARCHIVE}/{name}/{sha}"
    log(f"{repo}: deleting {name} at {sha[:7]} ({why})")
    # Its record stays until the listing drops it: a delete that didn't go
    # through is tried again next pass.
    delete_branch(repo, name, sha)
    return tip


# What only the owner can settle: their call (a label, a judge's or an
# outside contribution's), a login. The rest of the digest is the
# pipeline's own trouble (a job that keeps failing, a review that won't run,
# a merge GitHub won't take, a PR stuck a day, a branch it couldn't
# archive), which a Claude job on this machine takes when proj's inbox is on.
OWNERS = ("Judged yours to decide", "Outside contributions", "Too much to read", "A reviewer is out of service",
          "No reviewer can review it", "Flaky tests someone else quarantined")


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
        # Its job never waits in proj's line (the unstick label), which may be
        # the very thing stuck.
        issue("stuck.json", "[autoland] Stuck in the pipeline", CFG["inbox"]["label"], "", stuck_brief(),
              [x for x in digest if x not in owner], "Nothing stuck.", also=CFG["inbox"]["unstickLabel"])


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
this machine when it's the machine. Then get it moving again (`curl -fsS -d '' {CFG['kick']}/OWNER/REPO` starts a \
repo's pass). Your answer, posted here, says what you found and did for each. The steward keeps this issue up to date: \
something new stuck comes as a comment, and it closes the issue itself once nothing is, so don't close it. If one \
really does need {who} (a login only they can do, a call that's theirs), say exactly what and assign this issue to \
them.

"""


def issue(key, title, label, assignee, intro, digest, done, also=""):
    """One issue in config.notify.repo listing digest, kept up to date: each
    new or changed entry is a comment too, and it's closed once there's none.
    It's found by label, and has the label `also` as well, when there is one:
    put back on the open one when it's gone."""
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
                 f'"{title}" in:title', "--json", "number,body,labels", check=False)
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
        for name in (label, also):
            if name:
                C.gh("label", "create", name, "-R", nrepo, "--color", "B60205", check=False)
        args = ["issue", "create", "-R", nrepo, "--label", label, *(["--label", also] if also else []), "--title", title,
                "--body", full]
        if assignee:
            args += ["--assignee", assignee]
        ok = C.gh(*args, check=False) is not None
    else:
        num = str(found[0]["number"])
        if found[0]["body"] != full:
            ok = C.gh("issue", "edit", num, "-R", nrepo, "--body", full, check=False) is not None
        if also and ok and also not in {x.get("name") for x in found[0].get("labels") or []}:
            C.gh("label", "create", also, "-R", nrepo, "--color", "B60205", check=False)
            ok = C.gh("issue", "edit", num, "-R", nrepo, "--add-label", also, check=False) is not None
        added = [x for x in entries if x not in prev.get("entries", [])]
        if added and ok:
            ok = C.gh("issue", "comment", num, "-R", nrepo, "--body", "New:\n" + "\n".join(f"- {x}" for x in added), check=False) is not None
    if ok:
        save(key, {"entries": entries, "body": body, "checked": C.NOW})
