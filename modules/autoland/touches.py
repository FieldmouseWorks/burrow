"""What a PR touches, as the steward's gates read it: the one-way paths
(renames and copies included, from the diff), which dependency bot's update
it is and how big, and whether a bot's PR changes .github/ beyond repinning
actions or carries someone else's commits."""

import codecs
import re

import common as C
from common import CFG, DEP_RE


def one_way(pr, rc, moved=()):
    """The first path that makes this PR one a Claude job judges before it
    lands (config oneWay), or None. moved: where its renames and copies
    started (moved_from), None if that couldn't be read."""
    for path in [f["path"] for f in pr["files"]["nodes"]] + list(moved or ()):
        for pat in rc["oneWay"]:
            if re.search(pat, path):
                return path
    if pr["changedFiles"] > len(pr["files"]["nodes"]):
        return f"{pr['changedFiles']} files (too many to check)"
    if moved is None:
        return "a rename or copy whose diff couldn't be read"
    return None


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
    where each one went, so a rename out of a one-way path (CODEOWNERS to
    CODEOWNERS.bak) is read from the diff. Read once per head and base; None
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


def dep_kind(pr):
    """None, 'minor' or 'major' for a dependency bot's PR."""
    if not DEP_RE.match(pr["headRefName"]):
        return None
    labels = {x["name"] for x in pr["labels"]["nodes"]}
    if "security" in labels and pr["headRefName"].startswith("renovate/"):
        # Renovate's security fixes merge themselves, majors too: none waits
        # for renovate-sweep's verdict (renovate-sweep.sh).
        return "minor"
    m = MAJOR_RE.search(pr["title"])
    if "major" in labels or (m and m.group(1) != m.group(2)):
        return "major"
    return "minor"


# The address Dependabot commits as, on every repo.
DEPENDABOT_EMAIL = "49699333+dependabot[bot]@users.noreply.github.com"


def bot_edited(repo, pr, st):
    """Whether a dependency bot's branch has a commit no bot wrote (a Claude
    job's fix): code no reviewer has read, which Renovate won't merge either.
    Asked once per head. A failed ask is asked again: on a Renovate branch it
    counts as not edited meanwhile, since Renovate checks for itself before
    it merges; on a Dependabot one, which the steward merges, as edited."""
    renovate = pr["headRefName"].startswith("renovate/")
    if renovate and not CFG["renovateEmail"]:
        return False
    if st.get("editedHead") != pr["headRefOid"]:
        got = C.gh("pr", "view", str(pr["number"]), "-R", repo, "--json", "commits", check=False)
        if not got:
            return not renovate
        bots = {CFG["renovateEmail"], DEPENDABOT_EMAIL}
        st["edited"] = any(a.get("email") not in bots
                           for c in got["commits"] for a in c["authors"])
        st["editedHead"] = pr["headRefOid"]
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
    so a rename or copy is read from the diff too. (A list too long to see
    whole is already the owner's: one_way.) Read once per head and base; a diff that can't be read counts
    as an edit (reviewed, not merged on CI alone)."""
    nodes = pr["files"]["nodes"]
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
