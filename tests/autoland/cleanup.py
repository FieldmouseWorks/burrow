"""The branches a pass deletes because nothing needs them any more, on what
changes them (branches.py)."""

import json
import subprocess
import time
import urllib.parse

from harness import B, C, S, gh_stub

# Called straight here, so for sam's repos as a pass has them.
C._for.append("sam")
repo_r = {"repo": "sam/r", "private": False, "default": "main"}

# Branches nothing needs go on what changes them: a push, a PR closing, the
# default moving, or the wait they were given ending. One already in the
# default branch, or at the tip a closed PR was closed at (a bot's included:
# GitHub deletes on merge, never on close; the PR keeps the commits), goes
# once it has sat GRACE at that tip. One with no PR and no commit for
# STALE_DAYS is kept under refs/archive and deleted (one an earlier look
# archived too); one GitHub won't archive stays, listed for the pipeline.
# One pushed to since its PR closed, an open PR's, a bot's with no closed PR
# at its tip, the default and a kept one stay.
def ago(days):
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(C.NOW - days * 86400))


# Given up on a day past STALE_DAYS (3: agents open a PR when they push), or
# fresh from today.
old, young = ago(4), ago(1)
BRANCHES = {"main": ("ab" * 20, 0, young), "in-main": ("1" * 40, 0, young), "closed": ("2" * 40, 3, young),
            "moved": ("3" * 40, 1, young), "open": ("4" * 40, 2, old), "renovate/x": ("5" * 40, 1, old),
            "gh-pages": ("6" * 40, 9, old), "old": ("7" * 40, 4, old), "young": ("8" * 40, 4, young),
            "renovate/closed": ("9" * 40, 1, young), "renovate/again": ("a" * 40, 1, young),
            "archived": ("d" * 40, 2, old), "refused": ("e" * 40, 2, old)}
PROTECTED = set()  # the default here isn't, so it's listed with the rest
CLOSED = {"closed": [{"number": 3, "head": {"sha": "2" * 40}, "closed_at": ago(5)}],
          "moved": [{"number": 4, "head": {"sha": "f" * 40}, "closed_at": ago(5)}],
          # Its match is the 101st closed PR on the name: past gh's first page.
          "renovate/closed": [{"number": n, "head": {"sha": "c" * 40}, "closed_at": ago(5)} for n in range(100, 200)]
          + [{"number": 5, "head": {"sha": "9" * 40}, "closed_at": ago(5)}],
          # Its last PR merged at another tip, and the bot has pushed its next update.
          "renovate/again": [{"number": 6, "head": {"sha": "b" * 40}, "closed_at": ago(5)}]}
# The closed PRs as GitHub lists them by their last change: each one's
# branch, tip and repo.
FEED = [{"number": pr["number"], "updated_at": pr["closed_at"], "head": {"ref": name, "sha": pr["head"]["sha"], "repo": {"full_name": "sam/r"}}}
        for name, prs in CLOSED.items() for pr in prs]
LOOKED, FRESH, FED = [], [], []  # the branches whose closed PRs it read; what each listing got (200 or 304)
# What GitHub has under refs/archive, and what it won't take there.
ARCHIVED, REFUSED = {f"refs/archive/archived/{'d' * 40}": "d" * 40}, {"refused"}
# What GitHub answers instead: listing the branches or the closed PRs,
# comparing one (by name), or archiving one (by name).
LISTING, FEEDING, COMPARE, ARCHIVING = [None], [None], {}, {}


def no(k, why):
    """gh failing with `why`: None with check=False (gh logs it), else what
    gh raises."""
    if not k.get("check", True):
        return None
    raise (C.Unavailable if C.UNAVAILABLE.search(why) else RuntimeError)(f"gh api: {why}")


def close(name, number, sha, repo="sam/r"):
    """A PR from `name` (in `repo`) closed at `sha`, now: GitHub's times are
    whole seconds."""
    if repo == "sam/r":  # what head=sam:NAME lists
        CLOSED.setdefault(name, []).append({"number": number, "head": {"sha": sha}, "closed_at": ago(0)})
    FEED.append({"number": number, "updated_at": ago(0), "head": {"ref": name, "sha": sha, "repo": {"full_name": repo}}})


def branches_fresh(path, etag=None):
    if path.startswith("repos/sam/r/pulls?state=closed&sort=updated&direction=desc&per_page=100"):
        if FEEDING[0]:
            no({}, FEEDING[0])
        page = sorted(FEED, key=lambda p: p["updated_at"], reverse=True)[:100]
        tag = json.dumps(page)
        FED.append(304 if etag == tag else 200)
        return None if etag == tag else (tag, page)
    if path != "repos/sam/r/branches?per_page=100":
        return kept_fresh(path, etag)
    if LISTING[0]:
        no({}, LISTING[0])
    # What a listing shows: each branch's tip, not what it's compared to.
    tag = json.dumps([sorted((n, b[0]) for n, b in BRANCHES.items()), sorted(PROTECTED)])
    FRESH.append(304 if etag == tag else 200)
    if etag == tag:
        return None
    return tag, [{"name": n, "commit": {"sha": sha}, "protected": n in PROTECTED} for n, (sha, _, _) in BRANCHES.items()]


def branches_gh(*a, **k):
    path = (a[2] if a[1:2] == ("--paginate",) else a[1]) if a[:1] == ("api",) and len(a) > 1 else ""
    if path.startswith("repos/sam/r/compare/"):
        assert path.partition("compare/")[2].partition("...")[0] == repo_r["default"], path
        name, b = next((n, x) for n, x in BRANCHES.items() if x[0] == path.rpartition("...")[2])
        if name in COMPARE:
            return no(k, COMPARE[name])
        return {"ahead": b[1], "date": b[2]}
    if path.startswith("repos/sam/r/git/ref/"):
        got = ARCHIVED.get("refs/" + urllib.parse.unquote(path.removeprefix("repos/sam/r/git/ref/")))
        if got is None:
            return no(k, "HTTP 404: Not Found")
        return got == a[a.index("--jq") + 1].partition('== "')[2][:40]
    if path.startswith("repos/sam/r/pulls?state=closed"):
        # gh's --paginate --jq: each match's number and close, from every page, a line each.
        assert a[1] == "--paginate", a
        name = path.partition("head=sam:")[2]
        LOOKED.append(name)
        prs = CLOSED.get(name, [])
        if prs is None or isinstance(prs, str):
            return no(k, prs or "HTTP 403: Resource not accessible by integration")
        want = a[a.index("--jq") + 1].partition('.head.sha == "')[2][:40]
        return "\n".join(f'{pr["number"]} {pr["closed_at"]}' for pr in prs if pr["head"]["sha"] == want)
    return gh_stub(*a, **k)


def branches_write(*a, **k):
    if a[:2] != ("api", "repos/sam/r/git/refs"):
        return kept_write(*a, **k)
    f = dict(x.split("=", 1) for x in a[2:][1::2])
    if f["ref"].split("/")[2] in ARCHIVING:
        return no(k, ARCHIVING[f["ref"].split("/")[2]])
    if f["ref"] in ARCHIVED or f["ref"].split("/")[2] in REFUSED:
        return no(k, "HTTP 422: Reference already exists")
    ARCHIVED[f["ref"]] = f["sha"]
    return {"ref": f["ref"], "object": {"sha": f["sha"]}}


REFUSAL = "remote: error: GH013: Repository rule violations found for refs/heads/empty."


def deleted(repo, name, sha):
    """GitHub deleting it, unless it's one whose push is turned away: what
    it says then."""
    DELETED.append((name, sha))
    if name in UNDELETABLE:
        return REFUSAL
    del BRANCHES[name]
    return ""


def look(in_use=("open",), full=False):
    """A pass's look at sam/r's branches: what it lists for the pipeline."""
    digest = []
    DELETED.clear(), LOOKED.clear(), FRESH.clear(), FED.clear(), C.WAKE.clear()
    B.watch_branches(repo_r, set(in_use), digest, full)
    return [line for _, line, _ in digest]


def tips():
    return C.load("repo-sam_r.json", {})["tips"]


DELETED, UNDELETABLE = [], set()
kept_delete, kept_write, kept_fresh = B.delete_branch, C.write, C.fresh
B.delete_branch = deleted
S.C.CFG["keepBranches"] = ["gh-pages"]
C.gh, C.write, C.fresh = branches_gh, branches_write, branches_fresh
# The daily sweep's state, which nothing reads now.
C.save("repo-sam_r.json", {"branches": C.NOW - 86400, "unkept": ["x"], "stale": []})
# The first look judges every branch it may delete, and deletes none before
# GRACE: its pass wakes when that's over.
assert look(full=True) == [] and DELETED == [], DELETED
assert sorted(LOOKED) == ["archived", "closed", "in-main", "moved", "old", "refused", "renovate/again",
                          "renovate/closed", "renovate/x", "young"], LOOKED
assert C.WAKE == {"sam/r": C.NOW + B.GRACE}, C.WAKE
assert sorted(C.load("repo-sam_r.json", {})) == ["base", "branchesBy", "branchesEtag", "closedEtag", "closedSince",
                                                 "tips"], C.load("repo-sam_r.json", {})
# That pass, a kicked one: the unchanged listings cost nothing against the
# rate limit, and only what's due is judged again. What goes next is the
# unmerged one from today, STALE_DAYS from its last commit.
C.NOW += B.GRACE
assert look() == ["sam/r: `refused`"]
assert FRESH == FED == [304] and sorted(LOOKED) == ["archived", "closed", "in-main", "old", "refused",
                                                    "renovate/closed"], (FRESH, FED, LOOKED)
assert DELETED == [("in-main", "1" * 40), ("closed", "2" * 40), ("old", "7" * 40), ("renovate/closed", "9" * 40),
                   ("archived", "d" * 40)], DELETED
assert ARCHIVED == {f"refs/archive/archived/{'d' * 40}": "d" * 40, f"refs/archive/old/{'7' * 40}": "7" * 40}, ARCHIVED
assert C.WAKE == {"sam/r": C.ts(young) + B.STALE_DAYS * 86400}, C.WAKE
# Nothing changed since: a pass judges nothing (after the one that sees the
# deletes), and the one GitHub won't archive is all it lists. A full pass
# tries that one again.
look()
assert look() == ["sam/r: `refused`"] and FRESH == [304] and LOOKED == [] and DELETED == [], (FRESH, LOOKED, DELETED)
assert look(full=True) == ["sam/r: `refused`"] and LOOKED == ["refused"], LOOKED
# Its PR closing starts the look: an hour later, it goes. Reopened inside
# that hour, it stays.
close("open", 2, "4" * 40)
look(())
assert DELETED == [] and C.WAKE["sam/r"] == C.NOW + B.GRACE, (DELETED, C.WAKE)
C.NOW += B.GRACE
look()
assert DELETED == [] and tips()["open"] == {"sha": "4" * 40, "pr": True}
look(())
C.NOW += B.GRACE
look(())
assert DELETED == [("open", "4" * 40)], DELETED
# So does a PR opened and closed between two passes (their events came as
# one, or went missing): every pass reads the PRs closed since the last, at
# no cost while there are none. Not one from a fork's branch of that name,
# or closed at another tip than the branch's now.
close("renovate/x", 7, "5" * 40, repo="eve/r")
close("renovate/again", 8, "b" * 40)
look(())
assert FED == [200] and LOOKED == [], (FED, LOOKED)
close("renovate/x", 9, "5" * 40)
look(())
assert FED == [200] and LOOKED == ["renovate/x"] and C.WAKE["sam/r"] == C.NOW + B.GRACE, (FED, LOOKED, C.WAKE)
look(())
assert FED == [304] and LOOKED == [], (FED, LOOKED)
C.NOW += B.GRACE
look(())
assert DELETED == [("renovate/x", "5" * 40)], DELETED
# More closed since the last look than a page holds: every branch is judged
# again, from when it was first seen at its tip, since reading on by page
# number could skip one a reopen moves up a page. Only the one a PR closed
# at gets a new wait, an hour from that close.
BRANCHES["paged"] = ("ee" * 20, 1, young)
look(())
before = {name: tip.get("due") for name, tip in tips().items()}
C.NOW += 1
close("paged", 11, "ee" * 20)
for n in range(1000, 1100):
    C.NOW += 1
    close(f"gone-{n}", n, "0" * 40)
assert look(()) == ["sam/r: `refused`"] and FED == [200] and sorted(LOOKED) == sorted(tips()), (FED, LOOKED)
assert tips()["paged"]["due"] == C.NOW - 100 + B.GRACE, tips()["paged"]
assert {name: tip.get("due") for name, tip in tips().items() if name != "paged"} == \
    {name: due for name, due in before.items() if name != "paged"}, tips()
look(())
assert FED == [304] and LOOKED == [], (FED, LOOKED)
C.NOW += B.GRACE
look(())
assert DELETED == [("paged", "ee" * 20)], DELETED
# GitHub's times are whole seconds, the pass's clock isn't: a PR closed in
# the same second its branch (a bot's, with no wait of its own) was first
# seen still has it judged, and deleted an hour on.
BRANCHES["renovate/quick"] = ("e3" * 20, 1, young)
C.NOW += 0.25
look(())
assert "due" not in tips()["renovate/quick"], tips()
close("renovate/quick", 15, "e3" * 20)
look(())
assert LOOKED == ["renovate/quick"] and tips()["renovate/quick"]["due"] == C.NOW + B.GRACE, (LOOKED, tips())
C.NOW += B.GRACE
look(())
assert DELETED == [("renovate/quick", "e3" * 20)], DELETED
C.NOW += 0.75
# A PR opened and closed between passes, at the tip an older one closed at
# too: its hour runs from the newer close.
BRANCHES["twice"] = ("e4" * 20, 1, young)
CLOSED["twice"] = [{"number": 16, "head": {"sha": "e4" * 20}, "closed_at": ago(5)}]
look(())
C.NOW += 1800
close("twice", 17, "e4" * 20)
look(())
assert LOOKED == ["twice"] and tips()["twice"]["due"] == C.NOW + B.GRACE, (LOOKED, tips()["twice"])
C.NOW += B.GRACE
look(())
assert DELETED == [("twice", "e4" * 20)], DELETED
# A pass that stops partway and is tried again judges the closes it read
# again the same: each branch's hour runs from its PR's close, not from the
# pass, so failing passes can't keep putting it off.
BRANCHES.update({"first": ("f1" * 20, 1, young), "second": ("f2" * 20, 1, young)})
look(())
C.NOW += 1
closing = C.NOW
close("first", 12, "f1" * 20)
close("second", 13, "f2" * 20)
answers = CLOSED["second"]
CLOSED["second"] = "HTTP 502: Bad Gateway"
for _ in range(3):
    C.NOW += 600
    look(())
    assert LOOKED == ["first", "second"] and tips()["first"]["due"] == closing + B.GRACE, (LOOKED, tips()["first"])
CLOSED["second"] = answers
look(())
assert LOOKED == ["first", "second"] and tips()["second"]["due"] == closing + B.GRACE, (LOOKED, tips()["second"])
look(())
assert LOOKED == [], LOOKED  # read to the end once: those closes are behind it
C.NOW = closing + B.GRACE
look(())
assert DELETED == [("first", "f1" * 20), ("second", "f2" * 20)], DELETED
# So does a push: to the tip a closed PR was closed at, it goes an hour on.
BRANCHES["moved"] = ("f" * 40, 1, young)
look(())
assert LOOKED == ["moved"] and DELETED == [], (LOOKED, DELETED)
C.NOW += B.GRACE
look(())
assert DELETED == [("moved", "f" * 40)], DELETED
# A delete GitHub refuses (a ruleset, say) is listed for the pipeline with
# what GitHub said, and tried again on the full pass alone: not on every
# pass until then, nor when the default moves or a PR closes at it. It goes
# once GitHub lets it.
BRANCHES["empty"] = ("0" * 40, 0, old)
UNDELETABLE.add("empty")
look(())
C.NOW += B.GRACE
assert look(()) == ["sam/r: `empty`", "sam/r: `refused`"] and DELETED == [("empty", "0" * 40)], DELETED
digest = []
B.watch_branches(repo_r, set(), digest)
assert (B.UNDELETED, "sam/r: `empty`", REFUSAL) in digest, digest
assert look(()) == ["sam/r: `empty`", "sam/r: `refused`"] and DELETED == [], DELETED
BRANCHES["main"] = ("bc" * 20, 0, young)
assert look(()) == ["sam/r: `empty`", "sam/r: `refused`"] and DELETED == [], DELETED
BRANCHES["main"] = ("ab" * 20, 0, young)
look(())
# A PR closed at it days ago, changed now (a label), is in the closed feed.
CLOSED["empty"] = [{"number": 18, "head": {"sha": "0" * 40}, "closed_at": ago(5)}]
FEED.append({"number": 18, "updated_at": ago(0), "head": {"ref": "empty", "sha": "0" * 40, "repo": {"full_name": "sam/r"}}})
assert look(()) == ["sam/r: `empty`", "sam/r: `refused`"] and FED == [200] and DELETED == [], (FED, DELETED)
UNDELETABLE.clear()
look(full=True)
assert DELETED == [("empty", "0" * 40)], DELETED
assert look(()) == ["sam/r: `refused`"] and "empty" not in tips(), tips()
# The stale one's wait ending: archived and deleted.
C.NOW = C.ts(young) + B.STALE_DAYS * 86400
look(())
assert DELETED == [("young", "8" * 40)] and f"refs/archive/young/{'8' * 40}" in ARCHIVED, DELETED
# The default moving judges every branch again, at the tip it has sat at:
# one merged into it since goes as one already in it, unarchived, without
# waiting out STALE_DAYS. Those with no wait of their own (a bot's) are
# looked at too.
BRANCHES["later"] = ("1a" * 20, 2, ago(0))
look(())
assert tips()["later"]["due"] == C.NOW + B.STALE_DAYS * 86400, tips()["later"]
C.NOW += B.GRACE
BRANCHES.update({"main": ("ba" * 20, 0, young), "later": ("1a" * 20, 0, ago(0))})
look(())
assert FRESH == [200] and sorted(LOOKED) == sorted(tips()) and "renovate/again" in LOOKED, (FRESH, LOOKED)
assert DELETED == [("later", "1a" * 20)] and not [ref for ref in ARCHIVED if "/later/" in ref], (DELETED, ARCHIVED)
look(())
assert LOOKED == [], LOOKED  # the listing without it: the default is where it was
# A new default: the old one is judged like any other branch, against the
# new one, though the listing hasn't changed.
BRANCHES["trunk"] = ("cd" * 20, 1, ago(0))
look(())
repo_r["default"] = "trunk"
look(())
assert FRESH == [200] and "main" in LOOKED and "trunk" not in tips(), (FRESH, LOOKED)
C.NOW += B.GRACE
look(())
assert DELETED == [("main", "ba" * 20)], DELETED
repo_r["default"] = "main"
BRANCHES["main"] = BRANCHES.pop("trunk")
look(())
# GitHub not listing them (its rate limit, or a no), or the PRs closed since,
# changes nothing.
look(())
was = C.load("repo-sam_r.json", {})
for failing in (LISTING, FEEDING):
    for why in ("API rate limit exceeded for installation", "HTTP 403: Resource not accessible by integration"):
        failing[0] = why
        assert look(()) == ["sam/r: `refused`"] and C.load("repo-sam_r.json", {}) == was, why
    failing[0] = None
# Nor does GitHub not answering partway lose a branch or a closed PR: what
# it judged is kept, and the next pass lists both again (no ETag kept) and
# judges the rest. Archiving one then isn't one GitHub wouldn't archive.
BRANCHES.update({"new": ("0" * 40, 1, ago(0)), "newer": ("b" * 40, 1, ago(0))})
close("elsewhere", 10, "0" * 40, repo="eve/r")
COMPARE["newer"] = "HTTP 502: Bad Gateway"
look(())
now = C.load("repo-sam_r.json", {})
assert LOOKED == ["new", "newer"] and (now["branchesEtag"], now["closedEtag"]) == (was["branchesEtag"], was["closedEtag"]), LOOKED
del COMPARE["newer"]
look(())
assert FRESH == FED == [200] and LOOKED == ["newer"], (FRESH, FED, LOOKED)
BRANCHES["quiet"] = ("9" * 40, 2, old)
look(())
C.NOW += B.GRACE
ARCHIVING["quiet"] = "HTTP 502: Bad Gateway"
assert look(()) == ["sam/r: `refused`"] and DELETED == [], DELETED
del ARCHIVING["quiet"]
look(())
assert DELETED == [("quiet", "9" * 40)], DELETED
# A branch GitHub said no about, to its closed PRs or its comparison, waits
# for a full pass to be judged again: its no may be for now (its access) or
# for good (no history in common with the default, which reads the same).
# Once GitHub answers, it's judged as any other, from when it was first seen.
BRANCHES.update({"orphan": ("c" * 40, 1, old), "shy": ("e1" * 20, 1, old), "renovate/shy": ("e2" * 20, 1, old)})
COMPARE.update({"orphan": "HTTP 404: No common ancestor between main and ccccccc",
                "shy": "HTTP 403: Resource not accessible by integration"})
CLOSED["renovate/shy"] = None
look(())
seen = C.NOW
assert DELETED == [] and tips()["orphan"] == {"sha": "c" * 40, "seen": seen, "retry": True}, tips()["orphan"]
assert tips()["shy"]["retry"] and tips()["renovate/shy"]["retry"], tips()
look(())
assert LOOKED == [], LOOKED
del COMPARE["shy"]
CLOSED["renovate/shy"] = [{"number": 11, "head": {"sha": "e2" * 20}, "closed_at": ago(5)}]
C.NOW += B.GRACE
look(full=True)
assert sorted(LOOKED) == ["orphan", "refused", "renovate/shy", "shy"], LOOKED
assert DELETED == [("shy", "e1" * 20), ("renovate/shy", "e2" * 20)] and tips()["orphan"]["retry"], (DELETED, tips())
assert f"refs/archive/shy/{'e1' * 20}" in ARCHIVED and tips()["shy"]["seen"] == seen, ARCHIVED
# A first look that stops partway, tried again: it kept no base, so every
# branch is judged again from when it was first seen (a closed PR's from its
# close), and what it judged keeps its wait.
C.save("repo-sam_r.json", {})
BRANCHES["third"] = ("f3" * 20, 1, young)
COMPARE["third"] = "HTTP 502: Bad Gateway"
look(())
first = C.NOW
assert "third" not in tips() and "closedSince" not in C.load("repo-sam_r.json", {}), tips()
C.NOW += 600
close("renovate/again", 14, "a" * 40)
del COMPARE["third"]
look(())
assert tips()["renovate/again"]["due"] == C.NOW + B.GRACE and tips()["refused"]["seen"] == first, tips()
assert tips()["third"]["seen"] == C.NOW and "closedSince" in C.load("repo-sam_r.json", {}), tips()
# A branch kept no longer is judged, though the listing hasn't changed.
S.C.CFG["keepBranches"] = []
look(())
assert FRESH == [200] and LOOKED == ["gh-pages"], (FRESH, LOOKED)
# What git says when a delete doesn't go through, read whole: a push that
# beat it, or a branch already gone, is no refusal (the next listing shows
# either); GitHub not answering ends the pass; anything else is GitHub's
# refusal, in its words, on one line and cut short. A name git echoes (the
# branch, the repo, a URL) that reads like an outage doesn't make one.
run_kept = B.subprocess.run
long_repo = f"{'o' * 39}/{'r' * 100}"
for repo, name, said, want in (
        ("sam/r", "x", "! [rejected] x -> x (stale info)", ""),
        ("sam/r", "x", "error: unable to delete 'x': remote ref does not exist", ""),
        ("sam/r", "x", "remote: error: GH013: Repository rule violations found\nremote: - Cannot delete this branch", "refused"),
        ("sam/r", "x", "fatal: unable to access 'https://github.com/sam/r.git/': The requested URL returned error: 403", "refused"),
        ("sam/r", "x", "fatal: unable to access 'https://github.com/sam/r.git/': The requested URL returned error: 502", None),
        ("sam/r", "x", "fatal: unable to access 'https://github.com/sam/r.git/': Could not resolve host: github.com", None),
        ("sam/r", "x", "fatal: unable to access 'https://github.com/sam/r.git/': Failed to connect to github.com port 443: "
                       "Connection timed out", None),
        ("sam/r", "fix/timeout", "remote: error: GH013: Repository rule violations found for refs/heads/fix/timeout.\n"
                                 "To https://github.com/sam/r.git\n ! [remote rejected] fix/timeout (push declined due to "
                                 "repository rule violations)", "refused"),
        ("sam/timeout", "x", "fatal: unable to access 'https://github.com/sam/timeout.git/': The requested URL returned "
                             "error: 403", "refused"),
        ("sam/eof", "eof", "To https://github.com/sam/eof.git\n ! [remote rejected] eof (protected branch hook declined)\n"
                           "error: failed to push some refs to sam/eof", "refused"),
        (long_repo, "x", f"fatal: unable to access 'https://github.com/{long_repo}.git/': The requested URL returned "
                         "error: 502", None),
        ("sam/r", "x" * 250, f"! [rejected] {'x' * 250} -> {'x' * 250} (stale info)", "")):
    B.subprocess.run = lambda args, **k: subprocess.CompletedProcess(args, 1, "", said)
    try:
        got = kept_delete(repo, name, "1" * 40)
    except C.Unavailable:
        got = None
    assert got == (" ".join(said.split())[:200] if want == "refused" else want), (said, got)
B.subprocess.run = run_kept
# More than a page of branches: every page, and no ETag for next time. A
# protected one, the default too, isn't the steward's to delete.
C.fresh = lambda path, etag=None: ("tag", [{"name": f"b{i}", "commit": {"sha": "1" * 40}} for i in range(100)])
C.gh = lambda *a, **k: "\n".join([f"main {'2' * 40} true"] + [f"b{i} {'1' * 40} false" for i in range(101)]) \
    if a[2].startswith("repos/sam/r/branches?") else None
assert B.listed_branches("sam/r", "main", None) == (None, {f"b{i}": "1" * 40 for i in range(101)}, ["main", "2" * 40])
B.delete_branch, C.gh, C.write, C.fresh = kept_delete, gh_stub, kept_write, kept_fresh
S.C.CFG["keepBranches"] = []
C._for.pop()

print("cleanup: ok")
