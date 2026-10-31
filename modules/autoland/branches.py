"""The branch watch: deleting the branches nothing needs any more, on what
changes them (a push, a PR closing, the default moving, the wait it was
given ending), with a stale one's commits archived first."""

import fnmatch
import re
import subprocess
import tempfile
import urllib.parse

import common as C
import train as T
from common import CFG, DEP_RE, DRY, load, log, repo_cfg, save, ts


def delete_branch(repo, name, sha):
    """Delete the branch only if its tip is still `sha`: a push since then
    keeps it (git's lease; the REST API has no conditional delete). What
    GitHub said when it refused (a ruleset, a token that can't), else "":
    deleted, already gone, or kept by a push the next listing shows.
    C.Unavailable when GitHub didn't answer."""
    if DRY:
        log(f"  would: delete {repo} {name} if still at {sha[:7]}")
        return ""
    with tempfile.TemporaryDirectory() as d:
        subprocess.run(["git", "init", "-q", "--bare", d], check=False)
        p = subprocess.run(["git", "-C", d, *C.GIT_AUTH, "push", "-q", f"--force-with-lease=refs/heads/{name}:{sha}",
                            f"https://github.com/{repo}.git", f":refs/heads/{name}"],
                           capture_output=True, text=True, env=C.gh_env(repo))
    # Read whole, shown cut: a long URL mustn't push git's verdict off the end.
    said = " ".join(p.stderr.split())
    if p.returncode == 0 or re.search(r"stale info|remote ref does not exist", said):
        return ""
    # Less the fields where git echoes names back (URLs, refs, the names on
    # its ref-status lines), so a branch called fix/timeout reads as a
    # refusal, not an outage. Only those fields: a branch called `error`
    # mustn't take the word out of git's own "returned error: 502".
    bare = re.sub(r"https?://\S+|refs/\S+|\[[a-z ]+\] \S+", "", said)
    if C.UNAVAILABLE.search(bare) or re.search(r"could not resolve host|returned error: 5\d\d", bare, re.I):
        raise C.Unavailable(f"git push :{name}: {said[:200]}")
    log(f"{repo}: kept {name}: {said[:200]}")
    return said[:200] or f"git exited {p.returncode}"


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
    """(the closed PR whose head was this branch at `sha`, when it last
    closed), or 0 when there's none. GitHub keeps that PR's commits
    (refs/pull/N/head) and offers to restore the branch from its page, so
    the branch holds nothing the PR doesn't. None when GitHub said no;
    C.Unavailable when it didn't answer."""
    owner = repo.split("/")[0]
    # Every page: a branch name reused for many PRs can have its match
    # past the first hundred.
    got = answer(C.gh, "api", "--paginate", f"repos/{repo}/pulls?state=closed&per_page=100&head={owner}:{urllib.parse.quote(name)}",
                 "--jq", f'.[] | select(.head.sha == "{sha}") | "\\(.number) \\(.closed_at)"')
    if got is None:
        return None
    closes = [(ts(at), int(n)) for n, at in (x.split(" ") for x in str(got).split("\n") if x.count(" ") == 1)]
    return max(closes)[::-1] if closes else 0


def closed_heads(repo, since, etag):
    """The branches here, at the tips they closed at, of the PRs closed (or
    changed while closed) since `since`, the last look's newest change:
    (the etag to ask by next time, the newest change now, {(branch, tip)}).
    None while nothing has changed, a look GitHub doesn't count against
    the rate limit. No `since` (no look before): no heads, just where the
    next look starts; a pass that kept no look kept no base either, so the
    next judges every branch again (watch_branches). The heads are None
    when more changed since than a page holds, and every branch is to be
    judged again instead: reading on by page number could skip one a
    reopen moves up a page. RuntimeError when GitHub didn't list them."""
    got = C.fresh(f"repos/{repo}/pulls?state=closed&sort=updated&direction=desc&per_page=100", etag)
    if got is None:
        return None
    etag, page = got
    newest = max((ts(p["updated_at"]) for p in page), default=since or 0)
    # The same second as `since` again: one changed in it may not have been
    # listed then.
    newer = [p for p in page if since is not None and ts(p["updated_at"]) >= since]
    heads = None if len(newer) == 100 else {
        (p["head"]["ref"], p["head"]["sha"]) for p in newer if (p["head"].get("repo") or {}).get("full_name") == repo}
    return etag, newest, heads


UNKEPT = f"Stale branches it couldn't archive (unmerged, no PR, {STALE_DAYS}+ days)"
UNDELETED = "Branches GitHub wouldn't let it delete"


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
    stale one's archive, a delete, or a read a judgment needed. A pass GitHub stopped
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
        # (a merge into it, a force-push) may judge a branch otherwise. No
        # base kept (a first look, or one that stopped partway) reads as moved.
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
            elif was is None or was["sha"] != sha or was.get("pr"):
                # New, pushed to, or its PR just left the open ones: the wait starts now.
                tips[name] = judge(repo, r, name, sha, C.NOW)
            elif was.get("undeleted"):
                # GitHub refused its delete, which nothing but a push changes
                # (the default moving or a PR closing would only try it
                # again): tried again on the full pass alone.
                if full:
                    tips[name] = judge(repo, r, name, sha, was["seen"])
            elif heads is None or (name, sha) in heads or moved or looks_again(was, full):
                # Judged again from when it was first seen at this tip, so
                # its wait comes out the same however often it's judged: a
                # closed PR's runs from the close itself (judge).
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
        elif tip.get("undeleted"):
            digest.append((UNDELETED, f"{repo}: `{name}`", tip["undeleted"]))


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
    pr = closed_at(repo, name, sha)
    if pr is None:
        return {**tip, "retry": True}
    if pr:
        # An hour from the close, or from when it was first seen at this
        # tip if that's later: the same every time it's judged.
        due, why = max(seen, pr[1]) + GRACE, f"#{pr[0]} was closed at it, and keeps its commits"
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
    # Its record stays until the listing drops it. One GitHub refused is
    # listed for the pipeline and tried again on the full pass, not on every
    # pass until then.
    refused = delete_branch(repo, name, sha)
    return {**tip, "undeleted": refused} if refused else tip
