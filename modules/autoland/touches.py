"""What a PR touches, as the steward's gates read it: where its renames and
copies started (from the diff), which dependency bot's update it is and how
big, and whether a bot's PR changes .github/ beyond repinning actions or
carries someone else's commits. Whether it's R2 is Conary's class policy's
(train.py's gate)."""

import codecs
import re
import subprocess

import common as C
from common import CFG, DEP_RE


MOVED_RE = re.compile(r"^(?:rename|copy) from (.+)$")


def unquote(path):
    """A path as git's diff header has it: quoted, with C escapes, when it
    has odd characters. Bytes that aren't UTF-8 come back as U+FFFD, so the
    rest of the path (`secrets/`) still reads as it is."""
    if not (len(path) > 1 and path[0] == path[-1] == '"'):
        return path
    try:
        return codecs.escape_decode(path[1:-1].encode())[0].decode("utf-8", "replace")
    except ValueError:  # a broken escape: the path between the quotes, as written
        return path[1:-1]


def moved_from(repo, pr, st):
    """Where the PR's renames and copies started: the file list names only
    where each one went, so a rename out of the docs (docs/a.md to src/a.md)
    is read from the diff. Read once per head and base; None
    if the diff couldn't be read."""
    if not any(f.get("changeType") in ("RENAMED", "COPIED") for f in pr["files"]["nodes"]):
        return []
    seen = [pr["headRefOid"], pr["baseRefName"]]
    if st.get("moved", [None])[:2] != seen:
        diff = C.gh("pr", "diff", str(pr["number"]), "-R", repo, check=False)
        if not isinstance(diff, str) or not diff:
            return None
        st["moved"] = seen + [[unquote(m.group(1)) for m in map(MOVED_RE.match, diff.splitlines()) if m]]
    return st["moved"][2]


# "from 1.2.3 to 2.0.0", "from v3 to v4" (an action's major tag), "from 3 to 4".
MAJOR_RE = re.compile(r"from v?(\d+)\S* to v?(\d+)")


AUTOMERGE_RE = re.compile(r"\*\*Automerge\*\*: Enabled\b")


def automerges(pr):
    """Renovate merges this PR of its own once it's green: the configuration
    section it writes in the PR says so ("🚦 **Automerge**: Enabled.")."""
    return bool(AUTOMERGE_RE.search(pr.get("body") or ""))


STATUS_RE = re.compile(r"^\U0001f6a6 \*\*Automerge\*\*: (.*)$", re.M)
SPENT = "Disabled because a matching PR was automerged previously"


def automerge_spent(pr):
    """Renovate switched automerge off on this PR itself ("🚦 **Automerge**:
    Disabled because a matching PR was automerged previously."): it recreated
    a branch whose earlier PR it had automerged, which it never merges again.
    Read from the configuration status lines only (a line that starts with
    the signal), and only when every one says so: release notes Renovate
    embeds verbatim can quote the sentence, so any other status beside it
    (Renovate's own "Disabled by config", which is the owner's choice) is
    ambiguous and the PR keeps waiting."""
    statuses = STATUS_RE.findall(pr.get("body") or "")
    return bool(statuses) and all(x.startswith(SPENT) for x in statuses)


def dep_kind(pr):
    """None, 'minor' or 'major' for a dependency bot's PR."""
    if not DEP_RE.match(pr["headRefName"]):
        return None
    labels = {x["name"] for x in pr["labels"]["nodes"]}
    if "security" in labels and pr["headRefName"].startswith("renovate/"):
        # Renovate's security fixes merge themselves, majors too: none waits
        # for renovate-sweep's verdict (crates/burrow-renovate/src/sweep.rs).
        return "minor"
    m = MAJOR_RE.search(pr["title"])
    if "major" in labels or (m and m.group(1) != m.group(2)):
        return "major"
    return "minor"


# The address Dependabot commits as, on every repo.
DEPENDABOT_EMAIL = "49699333+dependabot[bot]@users.noreply.github.com"
# The address GitHub commits as for a commit it makes or signs itself, on
# github.com. Renovate doesn't count it as someone editing the branch.
GITHUB_EMAIL = "noreply@github.com"


def bot_edited(repo, pr, st, rechecked):
    """Whether a dependency bot's branch has a commit no bot wrote (a Claude
    job's fix) or that someone else committed (a Claude job's rebase keeps the
    author and makes itself the committer): code no reviewer has read, which
    Renovate won't merge either, since it counts both addresses of a commit.
    Where that's unknown (provenance), it counts as not edited where Renovate
    acts on the branch next (rechecked: it merges or rebases it), since it
    checks for itself first, and as edited where the steward merges it with
    nothing reading them after: a Dependabot update, or a Renovate one in a
    repo with its train on."""
    edited = provenance(repo, pr, st)
    return (not rechecked) if edited is None else edited


def whole(pages, head):
    """The commits a paged compare of head with the PR's base holds, each
    {sha, author, committer, trailers}, with the head among them; None if the
    read doesn't hold together (a page that isn't the compare's, pages that
    disagree on the merge base or the count because the base moved between
    them, a commit with no message), which is no evidence about this head.
    A str, why, if GitHub held together fewer commits than its own count:
    with the head and merge base fixed the range can't change, so that's
    GitHub's cap and no later read says more."""
    if not (isinstance(pages, list) and pages and all(isinstance(p, dict) for p in pages)):
        return None
    totals = {p.get("total_commits") for p in pages}
    forks = {(p.get("merge_base_commit") or {}).get("sha") for p in pages}
    commits = [c for p in pages for c in (p.get("commits") if isinstance(p.get("commits"), list) else [None])]
    if (len(totals) != 1 or len(forks) != 1 or None in forks
            or not all(isinstance(c, dict) and isinstance(c.get("sha"), str)
                       and isinstance((c.get("commit") or {}).get("message"), str) for c in commits)):
        return None
    total, shas = totals.pop(), {c["sha"] for c in commits}
    if not isinstance(total, int) or len(shas) != len(commits) or len(commits) > total:
        return None
    if len(commits) < total:
        return f"GitHub lists {len(commits)} of its {total} commits, so who wrote the rest can't be read"
    if head not in shas:
        return None
    return [{"sha": c["sha"],
             "author": (c["commit"].get("author") or {}).get("email"),
             "committer": (c["commit"].get("committer") or {}).get("email"),
             "trailers": subprocess.run(["git", "interpret-trailers", "--parse"], input=c["commit"]["message"],
                                        text=True, capture_output=True, check=True).stdout.splitlines()}
            for c in commits]


def pr_commits(repo, pr, st):
    """Every commit on the PR's head since it left its base, as whole() reads
    them, from GitHub's compare in pages: the PR's own commit list, and the
    compare unpaged, stop at 250. One read per head and base, shared by
    codex_written and provenance. The head goes in as its sha, so the read
    is of that head even if the PR moves meanwhile. A read that doesn't hold
    together isn't cached and is asked again next pass: a head whose
    authorship is unknown cannot land unchecked. GitHub's cap is cached."""
    seen = [pr["headRefOid"], pr["baseRefName"]]
    if st.get("commitsHead") != seen:
        pages = C.gh("api", "--paginate", "--slurp",
                     C.compare(repo, pr["baseRefName"], pr["headRefOid"]) + "?per_page=100", check=False)
        got = whole(pages, pr["headRefOid"])
        if got is None:
            return None
        for old in ("commitHead", "commitPages"):  # the PR commit list's pages, as read before
            st.pop(old, None)
        st["commitsHead"], st["commits"] = seen, got
    return st["commits"]


def codex_written(repo, pr, st):
    """Only actual trailers count, on any commit since the PR's base: True
    or False, cached per head and base. None if its commits couldn't be read
    this pass, retried next pass, so it can neither land nor spend a judge.
    Past GitHub's cap, why (a str): no one can say a Codex job didn't write
    it, so it's judged as if one did, never landed on its reviews alone."""
    seen = [pr["headRefOid"], pr["baseRefName"]]
    if st.get("codexHead") != seen:
        commits = pr_commits(repo, pr, st)
        if not isinstance(commits, list):
            return commits
        st["codexWritten"] = any(t.casefold() == "engine: codex" for c in commits for t in c["trailers"])
        st["codexHead"] = seen
    return st["codexWritten"]


BOTS_NAMED = "bots named"
CO_AUTHOR_RE = re.compile(r"^co-authored-by:.*<([^<>]*)>\s*$", re.I)


def co_authors(trailers):
    """The addresses a commit's Co-authored-by trailers name, as GitHub reads
    them; None for one that names none, which is no bot's either."""
    named = (CO_AUTHOR_RE.match(t) for t in trailers if t.casefold().startswith("co-authored-by:"))
    return [m.group(1) if m else None for m in named]


def provenance(repo, pr, st):
    """True if a dependency bot's branch has a commit some other author,
    co-author or committer made (bot_edited), False if every one is the bots'
    alone, None if that's unknown: its commits couldn't be read whole, or a
    Renovate branch on a machine with no Renovate address to check them
    against. Asked once per head, base and bots' addresses, since retargeting
    changes which commits are the PR's, and setting Renovate's address right
    changes whose they are. A failed ask is asked again."""
    renovate = pr["headRefName"].startswith("renovate/")
    if renovate and not CFG["renovateEmail"]:
        return None
    # A missing or empty address is no bot's: no commit is the bots' alone
    # for naming none, on a machine with no Renovate address least of all.
    bots = {e for e in (CFG["renovateEmail"], DEPENDABOT_EMAIL) if e}
    # GITHUB_EMAIL marks a read that looked at committers too, and BOTS_NAMED
    # one where an address no one gave counted as no bot's: a head read
    # before either is read again.
    seen = [pr["headRefOid"], pr["baseRefName"], CFG["renovateEmail"], DEPENDABOT_EMAIL, GITHUB_EMAIL, BOTS_NAMED]
    if st.get("editedHead") != seen:
        commits = pr_commits(repo, pr, st)
        if not isinstance(commits, list):
            return None
        st["edited"] = any(c["author"] not in bots or c["committer"] not in bots | {GITHUB_EMAIL}
                           or any(a not in bots for a in co_authors(c["trailers"])) for c in commits)
        st["editedHead"] = seen
    return st["edited"]


WORKFLOW_RE = re.compile(r"^\.github/workflows/[^/]+\.ya?ml$")
# A line that only names an action at a ref: "uses: owner/action@sha # v4.1.0".
# The ref is a sha, tag or branch and nothing else, so no shell can ride on it.
PIN_LINE_RE = re.compile(r"^(\s*(-\s+)?uses:\s*[^\s#@]+@)[0-9A-Za-z._/-]+(\s+#.*)?$")


def pins_only(repo, pr):
    """True if every file the PR's diff touches under .github/ is a workflow
    whose changes only repin actions: each removed `uses:` line is replaced in
    place by one that's the same up to its `@` (indentation, key and action),
    so it's the same step at a new ref, and nothing is added, dropped or moved
    (no permissions, triggers or commands, and no `uses:`-shaped text inside
    a `run:`). A rename or copy into or out of .github/, or a file with no
    repinned line (a mode change, a binary), is not a repin. None if the diff
    couldn't be read."""
    diff = C.gh("pr", "diff", str(pr["number"]), "-R", repo, check=False)
    if not isinstance(diff, str) or not diff:
        return None
    path, hunk, gated, pinned, out, back = None, False, set(), set(), [], []

    def paired():
        # A run of removed lines, then the run that replaced them, line by line.
        ok = len(out) == len(back) and all(a.group(1) == b.group(1) for a, b in zip(out, back))
        if out and ok:
            pinned.add(path)
        out.clear()
        back.clear()
        return ok

    for x in diff.splitlines():
        if not (hunk and x[:1] in "+-\\") or (x[:1] == "-" and back):
            if not paired():
                return False
        if x.startswith("diff --git "):
            a, sep, b = x[len("diff --git "):].partition(" b/")
            if not (sep and a.startswith("a/")):
                if ".github/" in x:
                    return False
                path = None
                continue
            a, path, hunk = a[2:], b, False
            if a.startswith(".github/") or b.startswith(".github/"):
                if a != b or not WORKFLOW_RE.match(b):
                    return False
                gated.add(b)
            else:
                path = None
        elif x.startswith("@@"):
            hunk = True
        elif path and hunk and x[:1] in "+-" and x[1:].strip():
            m = PIN_LINE_RE.match(x[1:])
            if not m:
                return False
            (out if x[0] == "-" else back).append(m)
    return paired() and gated <= pinned


def gate_edit(repo, pr, st):
    """Whether a dependency bot's PR changes .github/ beyond repinning the
    actions its workflows use: that's the gate itself, and a dependency
    update is never reviewed. The file list names only a rename's new path,
    so a rename or copy is read from the diff too. Read once per head and
    base; a list too long to see whole, or a diff that can't be read, counts
    as an edit (reviewed, not merged on CI alone)."""
    nodes = pr["files"]["nodes"]
    if pr["changedFiles"] > len(nodes):
        return True
    if not any(f["path"].startswith(".github/") or f["changeType"] in ("RENAMED", "COPIED") for f in nodes):
        return False
    if any(f["path"].startswith(".github/") and not WORKFLOW_RE.match(f["path"]) for f in nodes):
        return True
    seen = [pr["headRefOid"], pr["baseRefName"]]
    if st.get("pins", [None])[:2] != seen:
        pins = pins_only(repo, pr)
        if pins is None:
            return True
        st["pins"] = seen + [pins]
    return not st["pins"][2]
