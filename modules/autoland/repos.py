"""The repos: finding them, their settings and hooks (burrow-upkeep keeps
them at the baseline, and a full pass ends with its `hooks`), and the
issues that list what waits for the owner and what the pipeline is stuck
on. Deleting the branches nothing needs is burrow-upkeep's too (its
`branches`)."""

import fnmatch
import json
import os
import re
import shutil
import time

import common as C
import reviewers as V
from common import CFG, DRY, load, log, save
from tasks import RED_BRANCH, red_default_task, red_fix

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


def settle_repo(r, known):
    """The repo's labels, settings and hook, which burrow-upkeep keeps
    (crates/burrow-upkeep/src/settle.rs, hook.rs). Returns True if it's new:
    not in known (restart takes a remade one out). Raises when its settings
    couldn't be kept."""
    repo = r["repo"]
    name = f"repo-{repo.replace('/', '_')}.json"
    st = load(name, {})
    new = repo not in known
    # GitHub's databaseId for it, by which restart tells a remade repo: kept
    # once restart has passed it, settings kept before it was included. The
    # node id kept before it goes: it's no test of which repo this is.
    if r.get("id") and (st.get("databaseId") != r["id"] or "id" in st):
        st.pop("id", None)
        st["databaseId"] = r["id"]
        save(name, st)
    # An empty repo has no default branch yet: it gets its ruleset with its
    # first commit.
    out = C.upkeep("settle", repo, *(["--private"] if r["private"] else []),
                   *(["--empty"] if r["default"] is None else []))
    if out["failed"]:
        raise RuntimeError("; ".join(out["failed"]))
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


def watch_default(r, info, prs, digest):
    """A red default branch: CI failed on what's merged, and every PR is
    built on it. With no merge queue (docs/repo-baseline.md), this is how
    two PRs that broke it together get undone: a Claude job finds the
    cause and opens a PR from RED_BRANCH that reverts or fixes it, which
    lands like any other. Nothing more while that job works or a fix is
    open; a second job if it's still red after (seen on two passes), then
    the owner. A fix lands only while CI says the branch is red: while it
    says nothing yet (the branch moved on, a fix landed, say, and its CI is
    running) a fix waits, and once it's green again a fix isn't needed and
    is closed. Returns the fixes this pass mustn't merge, each with why;
    step.handle holds the rest too if the branch has moved on since this
    read it (a merge earlier in this very pass)."""
    repo, default = r["repo"], r["default"]
    target = (info.get("defaultBranchRef") or {}).get("target") or {}
    sha, state = target.get("oid"), (target.get("statusCheckRollup") or {}).get("state")
    key = f"repo-{repo.replace('/', '_')}.json"
    st = load(key, {})
    red = st.get("red") or {}
    working = red.get("job") and C.job_state(red["job"]) in (None, "queued", "running")
    fixes = [pr for pr in prs if red_fix(pr, default)]
    if state not in ("FAILURE", "ERROR", "SUCCESS"):
        # CI on the branch hasn't said (it's running, or has nothing on the
        # tip): it may have gone green since it was red. A fix waits for it,
        # or one could land on top of the fix that made it green, and a
        # revert would undo that.
        return {pr["number"]: f"Waiting for CI on {default}: it lands only while {default} is red" for pr in fixes}
    if state == "SUCCESS":
        if not (red or fixes):
            return {}
        gone = f"Not needed: {default} is green again without it"
        # Green while its job still works: wait for the job to end, so a PR
        # it opens can't outlive the red it was for, and leave the fixes
        # alone till then. Once it has ended, no fix is needed.
        if working:
            return {pr["number"]: gone for pr in fixes}
        # Ended with no PR to show: as when red, seen on two passes before
        # the red is forgotten, since this one may have read the PRs just
        # before the job opened its own.
        if not fixes and red.get("job") and not red.get("ended"):
            red["ended"] = True
            save(key, st)
            C.wake(repo, C.NOW + 60)  # ends: the second look, once
            return {}
        unclosed = []
        for pr in fixes:
            log(f"{repo}: closing #{pr['number']}, {default} is green again without it")
            if C.write("pr", "close", str(pr["number"]), "-R", repo, "--comment",
                       f"{default} is green again at {sha[:7]}, so this fix for {pr['headRefName'].removeprefix(RED_BRANCH)[:7]} "
                       "isn't needed.", check=False) is None:
                unclosed.append(pr)
        # One GitHub wouldn't close is still known by its branch, and tried
        # again soon.
        if unclosed:
            C.wake(repo, C.NOW + 5 * 60)  # ends: the fix PR's own wait is overdue at its deadline (waits.py)
        elif red:
            log(f"{repo}: {default} is green again")
            st.pop("red")
            save(key, st)
        return {pr["number"]: gone for pr in fixes}
    line = f"{repo} {default} at {sha[:7]}"
    if working or fixes:
        return {}
    # Its job ended with no PR to show. Seen on two passes: this one may
    # have read the PRs just before the job opened its own.
    if red.get("job") and not red.get("ended"):
        red["ended"] = True
        save(key, st)
        C.wake(repo, C.NOW + 60)  # ends: the second look, once
        return {}
    if red.get("jobs", 0) >= 2:
        digest.append(("The default branch is red", line, "two Claude jobs ended with it still red"))
        return {}
    jid = C.job(repo, red_default_task(repo, default, sha))
    if not jid:
        digest.append(("Couldn't start a Claude job to fix it", line, "the default branch is red"))
        C.wake(repo, C.NOW + 5 * 60)  # ends: listed on the stuck issue
        return {}
    log(f"{repo}: {default} is red at {sha[:7]}; a Claude job {jid} fixes it")
    st["red"] = {"job": jid, "jobs": red.get("jobs", 0) + 1}
    save(key, st)
    return {}


def hooks(repos):
    """A full pass's hooks, burrow-upkeep's (crates/burrow-upkeep/src/hook.rs):
    the projects' repos' it doesn't see to, the ones off the repos it no
    longer sees to, and the App's own, each as the App's installation on its
    owner, which it mints for itself. It's handed the repos the pass saw to
    and the ones the projects are clones of. What wouldn't save is logged:
    the next full pass tries it again. A status run has no hooks to change."""
    if DRY:
        return
    out = C.upkeep("hooks", mints=True, input=json.dumps({
        "seen": [r["repo"] for r in repos], "projects": [repo for _, repo in C.projects()]}))
    for f in out["failed"]:
        log(f"the hooks: {f}")


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
