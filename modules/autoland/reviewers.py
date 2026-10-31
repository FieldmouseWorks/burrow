"""The steward's reviewers: Codex (the codex CLI), an agent that reads a
snapshot of the PR and answers with its findings.

A pass takes the snapshot and queues the review (`queue`); the reviewers'
service (review_worker.py) runs the agents as a user of their own, with no
GitHub access and no secrets paths in what they read; the next pass posts
what came back (`post`), as the steward's App and marked as that
reviewer's, and reads each reviewer's verdict back from those posts
(`verdict`)."""

import fcntl
import hashlib
import json
import os
import re
import shutil
import subprocess
import tempfile
import time

import common as C
from common import CFG, DRY, MIN, STATE, load, log, save, ts

NAMES = {"codex": "Codex"}
# Under STATE: what's asked for, what each snapshot keeps of its base (its
# NAME.json: the review rules, and the paths the reviewers weren't shown,
# which a judge looks at before the PR lands), what came back, and a clone
# per repo the snapshots are taken from.
QUEUE, SNAP, DONE, GIT = "review/queue", "review/snap", "review/done", "review/git"
# Each snapshot's checkout, the tree a reviewer reads: rewritten for every
# head and taken again from GitHub whenever it's missing, so it's a cache,
# kept out of STATE and whatever snapshots the machine keeps of its state
# (a big repo's runs to the best part of a gigabyte a head). Absolute, so
# os.path.join(STATE, TREES) is TREES.
TREES = CFG["snapDir"]
# Each reviewer's session for each open PR, which every look at it resumes.
# The reviewers' service owns what's in them; the sweep marks a closed PR's
# GONE, and the service deletes it.
SESSIONS, GONE = "review/sessions", ".gone-"
ORIGIN = "https://github.com/{}.git"
OUT = "reviewers.json"  # a reviewer out of service, and until when
META = {"rules", "house", "secret"}  # what a snapshot keeps of its base, for the prompt
# How the reviewers and a judge weigh the reasons a PR's description gives
# where the house rules ask for one. That the line is there is CI's to check;
# whether its reason holds up takes reading the code.
REASONS = ("Where the house rules ask a PR for a line in its description giving a reason (one starting `Why not Rust:` "
           "for new code that isn't Rust, say), judge the reason against the rule it answers and against the code, not only "
           "that it's there. Rust is the default, and a coin toss goes to Rust: a reason that only says it was quicker, that "
           "it's small, that the code around it is in that language already, or that the code itself contradicts (a "
           "\"throwaway\" script a service runs, \"glue\" with logic of its own) doesn't hold.")


def on(repo):
    """The reviewers this repo has, in a fixed order."""
    rc = C.repo_cfg(repo)
    return [r for r in NAMES if CFG["reviewers"][r]["enable"] and rc[r]]


def out(r):
    """Why the reviewer is out of service (usage spent, not logged in), or
    None. Out, it isn't asked and isn't waited for."""
    o = load(OUT, {}).get(r)
    return o["why"] if o and C.NOW < o["until"] else None


def review_re(r):
    return re.compile(rf"<!-- {r}-review:v1 (\{{.*?\}}) -->")


def finding(r):
    return f"<!-- {r}-finding -->"


def described(pr, seen):
    """Whether a review that was shown the description `seen` (its
    fingerprint) was shown the PR's as it is now. The reasons a description
    gives are part of what a reviewer judges, so a review of another one
    doesn't cover the PR: a `Why not Rust:` line written in after a clean
    review (CI wanted one) has had nobody weigh it."""
    return seen == C.fingerprint(pr["body"])


def verdict(pr, head, r):
    """What the reviewer last said about this exact head on the PR's base,
    shown its description as it is now (clean, findings or none), when, and
    how many of its reviews on the PR had findings. Only reviews the steward
    posted count: anyone can write the marker."""
    pat = review_re(r)
    v, at, rounds = "none", 0, 0
    for x in pr["reviews"]["nodes"]:
        m = pat.search(x["body"] or "")
        if not m or not C.mine(x["author"]):
            continue
        try:
            said = json.loads(m.group(1))
            found = int(said.get("findings", 0)) > 0
        except (ValueError, TypeError, AttributeError):
            continue
        rounds += found
        # Retargeted, the PR is another change on the same head: what was
        # said about it on its old base doesn't count.
        if (said.get("commit_sha") == head and said.get("base") == pr["baseRefName"] and described(pr, said.get("description"))
                and (x["commit"] or {}).get("oid") == head and ts(x["submittedAt"]) >= at):
            v, at = ("findings" if found else "clean"), ts(x["submittedAt"])
    return v, at, rounds


# ---------------------------------------------------------------- the snapshot

def names(out):
    """Paths from git's -z output. A path is bytes, not always UTF-8: the
    ones that aren't keep their bytes (surrogateescape), so git gets the
    same path back."""
    return [p for p in out.decode(errors="surrogateescape").split("\0") if p]


def shown(path):
    """A path as text to show (a prompt, a judge's task): any bytes that
    aren't UTF-8 as replacement characters."""
    return path.encode(errors="surrogateescape").decode(errors="replace")


def git(*args, cwd=None, input=None, env=None):
    p = subprocess.run(["git", *args], cwd=cwd, input=input, env=env, capture_output=True, check=False)
    if p.returncode != 0:
        raise RuntimeError(f"git {' '.join(args[:2])}: {p.stderr.decode(errors='replace').strip()[:300]}")
    return p.stdout


def skipped(repo, path):
    """The machine's skipPaths, and the repo's own on top: a repo's list
    only adds to the machine's, so nothing can take a secrets path back
    into view."""
    return any(re.search(p, path) for p in CFG["skipPaths"] + C.repo_cfg(repo)["skipPaths"])


def hidden(repo, path):
    """A secrets path: matched itself, or in a directory that is (a
    directory matches with a trailing slash)."""
    parts = path.split("/")
    return skipped(repo, path) or any(skipped(repo, "/".join(parts[:i]) + "/") for i in range(1, len(parts)))


def rules(repo, mirror, rev, paths):
    """The `## Code Review Rules` of the AGENTS.md nearest each changed file,
    and the `## House rules` the reasons a PR's description gives answer to,
    from every AGENTS.md from the root down to each changed file (a nearer
    one can add to the root's), as `rev` (the base branch) has them: a PR
    can't rewrite its own rules, and the base it lands on is the one whose
    rules hold. An AGENTS.md in a secrets path is never read: its rules
    would reach the reviewers in the prompt. Returns the two, as text."""
    have = {f for f in names(git("-C", mirror, "ls-tree", "-r", "-z", "--name-only", rev)) if not hidden(repo, f)}
    nearest, above = set(), set()
    for p in paths:
        mine, d = [], p
        while d:
            d = d.rpartition("/")[0]
            f = f"{d}/AGENTS.md" if d else "AGENTS.md"
            if f in have:
                mine.append(f)
        nearest.update(mine[:1])
        above.update(mine)
    out = {"Code Review Rules": [], "House rules": []}
    for f in sorted(above):
        text = git("-C", mirror, "show", f"{rev}:{f}").decode(errors="replace")
        for heading, found in out.items():
            m = re.search(rf"^## {heading}\s*$(.*?)(?=^## |\Z)", text, re.M | re.S)
            if m and (f in nearest or heading == "House rules"):
                found.append(f"From {f}:\n{m.group(1).strip()}")
    return ["\n\n".join(x) for x in out.values()]


def fetch(repo, *refs):
    """The steward's bare clone of the repo, which only it reads, with refs
    fetched into it from GitHub: what the snapshots are taken from, and what
    a proof's builds are worked out from (proofs.py)."""
    mirror = os.path.join(STATE, GIT, repo.replace("/", "__") + ".git")
    if not os.path.isdir(mirror):
        git("init", "-q", "--bare", mirror)
    git("-C", mirror, *C.GIT_AUTH, "fetch", "-q", "--no-tags", ORIGIN.format(repo), *refs, env=C.gh_env(repo))
    return mirror


def snap_name(key, head, base):
    """One snapshot a head on a base: retargeted, a PR is another change."""
    return f"{key[:-5]}-{head[:12]}-{hashlib.sha1(base.encode()).hexdigest()[:8]}"


def remove(p):
    """p gone, whether this takes it or something else (the reviewers'
    service, done with a queued review) got there first."""
    try:
        if os.path.isdir(p) and not os.path.islink(p):
            shutil.rmtree(p)
        else:
            os.remove(p)
    except FileNotFoundError:
        pass


def snapshot(repo, pr, key):
    """A fresh repo of two commits, the PR's merge base and its head, each
    without the secrets paths: all a reviewer gets to read. The commits are
    git's own trees with those paths taken out, so nothing the repo's
    attributes or ignore files say can change or hide what the PR changed.
    Taken once a head on a base, for every reviewer. Returns its name, the
    review rules and the secrets paths the PR changes."""
    n, head, base = pr["number"], pr["headRefOid"], pr["baseRefName"]
    name = snap_name(key, head, base)
    meta = load(f"{SNAP}/{name}.json", None)
    dst = os.path.join(TREES, name)
    # One kept by a steward that read less of the base is taken again, and
    # so is one whose checkout the cache lost (a reboot, say).
    if meta and META <= meta.keys() and os.path.isdir(dst):
        return name, meta
    mirror = fetch(repo, f"+refs/pull/{n}/head:refs/pr/{n}", f"+refs/heads/{base}:refs/base/{base}")
    mb = git("-C", mirror, "merge-base", f"refs/base/{base}", head).decode().strip()
    changed = names(git("-C", mirror, "diff", "-z", "--name-only", "--no-renames", mb, head))
    # A secret renamed or copied to a plain path is still a secret, so its
    # new name is hidden too. A plain file copied into a secrets path stays
    # in view: only the copy is hidden, by its own path.
    moved = set()
    out = names(git("-C", mirror, "diff", "-z", "--name-status", "-M", "-C", "--find-copies-harder", mb, head))
    i = 0
    while i < len(out):
        status = out[i]
        if status[:1] in ("R", "C"):
            src, dst_ = out[i + 1], out[i + 2]
            if hidden(repo, src):
                moved.add(dst_)
            i += 3
        else:
            i += 2
    # Each commit's tree less the secrets paths, built in the mirror (which
    # only the steward reads), so their contents never reach the snapshot.
    who = ["-c", "user.name=autoland", "-c", "user.email=autoland@localhost", "-c", "commit.gpgsign=false"]
    index = os.path.join(mirror, f"snap-{name}.index")
    parent = []
    empty = tempfile.mkdtemp()
    try:
        for label, rev in (("base", mb), (f"head {head}", head)):
            if os.path.exists(index):
                os.remove(index)
            # The index only: git wants a work tree for it, and an empty one
            # is never read or written.
            e = {**os.environ, "GIT_INDEX_FILE": index, "GIT_WORK_TREE": empty}
            subprocess.run(["git", "-C", mirror, "read-tree", rev], env=e, check=True, capture_output=True)
            paths = names(subprocess.run(["git", "-C", mirror, "ls-files", "-z"], env=e, check=True,
                                         capture_output=True).stdout)
            gone = [p for p in paths if hidden(repo, p) or p in moved]
            if gone:
                subprocess.run(["git", "-C", mirror, "update-index", "-z", "--force-remove", "--stdin"], env=e,
                               input="\0".join(gone).encode(errors="surrogateescape") + b"\0", check=True, capture_output=True)
            tree = subprocess.run(["git", "-C", mirror, "write-tree"], env=e, check=True,
                                  capture_output=True).stdout.decode().strip()
            commit = git("-C", mirror, *who, "commit-tree", tree, *parent, "-m", label).decode().strip()
            parent = ["-p", commit]
    finally:
        os.rmdir(empty)
        if os.path.exists(index):
            os.remove(index)
    git("-C", mirror, "update-ref", f"refs/snap/{name}", commit)
    # Made aside and moved into place whole: a checkout under its own name
    # is always a finished one, which the meta kept from before may vouch for.
    new = dst + ".new"
    shutil.rmtree(new, ignore_errors=True)
    # Nothing from the steward's own git config (an LFS filter, say) runs
    # on what the PR put there.
    bare = {**os.environ, "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1"}
    git("init", "-q", new, env=bare)
    git("-C", new, "fetch", "-q", "--no-tags", mirror, f"refs/snap/{name}:refs/heads/review", env=bare)
    git("-C", mirror, "update-ref", "-d", f"refs/snap/{name}")
    git("-C", new, "checkout", "-q", "review", env=bare)
    shutil.rmtree(dst, ignore_errors=True)
    os.rename(new, dst)
    review, house = rules(repo, mirror, f"refs/base/{base}", [p for p in changed if not (hidden(repo, p) or p in moved)])
    meta = {"rules": review, "house": house, "secret": [shown(p) for p in changed if hidden(repo, p) or p in moved]}
    save(f"{SNAP}/{name}.json", meta)
    return name, meta


# ---------------------------------------------------------------- earlier findings

def prior(repo, n, r):
    """The reviewer's earlier findings on this PR and the replies each got."""
    got = C.gh("api", "--paginate", f"repos/{repo}/pulls/{n}/comments?per_page=100", "--jq",
               ".[] | {id, in_reply_to_id, path, line, original_line, body, user: .user.login}", check=False)
    cs = []
    for x in (got if isinstance(got, str) else json.dumps(got) if got else "").splitlines():
        try:
            cs.append(json.loads(x))
        except ValueError:
            pass
    mark = finding(r)
    roots = [c for c in cs if c["user"] == C.me() and mark in (c["body"] or "")]
    text = []
    for c in roots:
        # Only replies from those whose PRs it reviews: anyone else's text
        # never steers it.
        replies = [x for x in cs if x.get("in_reply_to_id") == c["id"]
                   and (C.rest_ours(x["user"]) or x["user"] == C.me())]
        at = c.get("line") or c.get("original_line")
        text.append(f"- {c['path']}{f':{at}' if at else ''}: {c['body'].replace(mark, '').strip()}\n"
                    + "".join(f"  Reply from {x['user']}: {x['body'].strip()}\n" for x in replies))
    return "\n".join(text)


def session_name(r, snap):
    """The reviewer's session for the PR, on its base, whose head the
    snapshot `snap` is: the same for every head, so each look resumes the
    last one's, and another for a retargeted PR, which is another change."""
    pr, _, base = snap.rsplit("-", 2)
    return f"{r}--{pr}-{base}"


def item_name(key, r, head, base):
    return f"{r}--{snap_name(key, head, base)}"


def queue(repo, pr, key, r):
    """Queue the reviewer on the head's snapshot, with the PR's description
    as it is now: one edited to answer a finding is what the next look at
    the same head reads. In line by when the PR was opened (opened), so one
    pushed again keeps its place. Raises if the snapshot can't be taken (the
    head moved on, GitHub's unreachable)."""
    snap, meta = snapshot(repo, pr, key)
    save(f"{QUEUE}/{r}--{snap}.json", {
        "reviewer": r, "snap": snap, "repo": repo, "number": pr["number"], "headRefOid": pr["headRefOid"],
        "baseRefName": pr["baseRefName"], "title": pr["title"], "body": pr["body"],
        "rules": meta["rules"], "house": meta["house"], "secret": meta["secret"],
        "prior": prior(repo, pr["number"], r), "opened": ts(pr["createdAt"]), "at": C.NOW})


def ask(repo, pr, key, r, rs, after):
    """Queue the reviewer on the PR's head, unless it's already on its way.
    Its looks at one head are bounded by `rounds`: each one with findings
    counts as a round. rs is the reviewer's state on the PR, which keeps
    its tries; after books the PR's next pass (step.py's)."""
    head = pr["headRefOid"]
    tag = f"{repo}#{pr['number']}"
    if rs.get("queued"):
        return
    if DRY:
        log(f"  would: ask {NAMES[r]} to review {tag} at {head[:7]}")
        return
    if rs.get("fails", 0) >= 3 and C.NOW - rs.get("failedAt", 0) > 3600:
        rs["fails"] = 0  # an outage passes: three more tries an hour on
    if rs.get("fails", 0) >= 3:
        after(rs.get("failedAt", 0), 60)  # ends: listed (stuck, step.py), three more tries an hour on
        return
    if rs.get("fails") and C.NOW - rs["failedAt"] < 5 * MIN:
        # Tries five minutes apart, however many events come meanwhile:
        # a hiccup gets time to pass before all three are spent.
        after(rs["failedAt"], 5)  # ends: three tries, then hourly, and listed
        return
    try:
        queue(repo, pr, key, r)
    except Exception as e:  # the snapshot: GitHub, git, the disk
        rs["fails"], rs["failedAt"] = rs.get("fails", 0) + 1, C.NOW
        log(f"{tag}: couldn't queue {NAMES[r]} on {head[:7]} ({rs['fails']}): {str(e)[:200]}")
        after(C.NOW, 5)  # ends: three tries, then hourly, and listed
        return
    log(f"{tag}: queued a {NAMES[r]} review of {head[:7]}")
    rs["queued"] = C.NOW


def unseen(key, head, base):
    """What the head changes that its snapshot hid from the reviewers
    (secrets paths, and plain ones a secret was moved to). [] until the
    snapshot's taken."""
    return (load(f"{SNAP}/{snap_name(key, head, base)}.json", None) or {}).get("secret", [])


def drop(name):
    """A review's queue entry and answer."""
    for p in (f"{QUEUE}/{name}.json", f"{DONE}/{name}.json"):
        try:
            os.remove(os.path.join(STATE, p))
        except OSError:
            pass


def mine(d, key, r):
    """The names of the reviewer's entries in `d` (the queue, the answers),
    for any head, for the PR whose state file is `key`: parsed, not matched
    by prefix, since sam/r-5's names start with sam/r PR 5's. [] if there's
    no such directory (a status run that couldn't make it)."""
    try:
        names = [x.removesuffix(".json") for x in os.listdir(os.path.join(STATE, d))]
    except OSError:
        return []
    return sorted(n for n in names if n.partition("--")[0] == r and n.partition("--")[2].rsplit("-", 2)[0] == key[:-5])


def answered(key, r):
    """The names of the reviewer's answers, for any head, to the PR."""
    return mine(DONE, key, r)


def in_queue(key, r, base):
    """The names of the reviewer's looks at the PR on `base`, for any head,
    still in the queue, begun or not."""
    on = snap_name(key, "", base).rsplit("-", 1)[1]
    return [name for name in mine(QUEUE, key, r) if name.rsplit("-", 1)[1] == on]


def under_way(key, r, base):
    """The names of the reviewer's looks at the PR on `base`, for any head,
    that the reviewers' service is on now (on another base, the PR was
    another change). Those it hasn't begun are taken off the queue
    (`taken`), so none begins after this."""
    on = snap_name(key, "", base).rsplit("-", 1)[1]
    return [name for name in mine(QUEUE, key, r)
            if name.rsplit("-", 1)[1] == on and not taken(os.path.join(STATE, QUEUE, name + ".json"))]


def taken(path):
    """Takes the queued review at path off the queue unless the reviewers'
    service is on it (it holds the entry's lock while it is: review_worker
    `review`). False if it is, or if it can't be opened to tell."""
    try:
        f = open(path)
    except FileNotFoundError:
        return True
    except OSError:
        return False
    with f:
        try:
            fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return False
        remove(path)  # under the lock: the service, waiting on it, sees it gone
        return True


def sweep(repos, live):
    """For the repos this pass read (`repos`, as state file names spell
    them): the snapshots, queue entries and answers of heads none of their
    open PRs has any more (`live`: the snapshot names of the heads they have).
    An open PR's earlier head keeps what came back, for the PR's next pass to
    post on the commit it read or drop (review_step.py's `overtaken`). A look the
    reviewers' service is on stays, whoever's it is, with the snapshot it
    reads and the session it's in, until it ends: so nothing of it is left
    to write after the sweep, whatever code the service runs (it need only
    hold its entry's lock, `taken`), and a later sweep takes the rest.
    Returns what it left of the heads it swept: their looks under way, and
    the sessions it couldn't mark gone."""
    prs = {s.rsplit("-", 2)[0] for s in live}
    keep, busy, left = set(), set(), []
    for d in (QUEUE, DONE, SNAP, TREES):  # the queue first: a look still running keeps its snapshot
        for x in os.listdir(os.path.join(STATE, d)):
            snap = x.removesuffix(".json")
            if d in (QUEUE, DONE):
                snap = snap.partition("--")[2]
            if snap.rsplit("-", 3)[0].removeprefix("pr-") not in repos or snap in live:
                continue
            if d == QUEUE:
                if not taken(os.path.join(STATE, d, x)):
                    keep.add(snap)
                    busy.add(session_name(x.partition("--")[0], snap))
                    left.append(x)
            elif not (snap in keep if d in (SNAP, TREES) else snap.rsplit("-", 2)[0] in prs):
                remove(os.path.join(STATE, d, x))
    # And the reviewers' sessions, for good, once their PR is closed (or
    # retargeted): nothing of one PR's carries over to another's.
    on = {f"{pr}-{base}" for pr, _, base in (s.rsplit("-", 2) for s in live)}
    for x in os.listdir(os.path.join(STATE, SESSIONS)):
        pr = x.partition("--")[2]
        if x.startswith(GONE) or pr.rsplit("-", 2)[0].removeprefix("pr-") not in repos or pr in on or x in busy:
            continue
        try:
            os.rename(os.path.join(STATE, SESSIONS, x), os.path.join(STATE, SESSIONS, f"{GONE}{time.time_ns()}-{x}"))
        except OSError as e:
            log(f"couldn't mark the session {x} gone: {e}")
            left.append(x)
    return left


# ---------------------------------------------------------------- posting

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


def file_path(lines):
    """For one file's part of a diff: the name it has on the new side (the
    old one if it's deleted), or None if that's not certain."""
    pairs = header_pairs(lines[0])
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
            return None  # a name it can't read
        told[m.group(1) + (m.group(2) if m.group(1) in ("rename", "copy") else "")] = name
    same = {b for a, b in pairs if a == b}
    return (told.get("+++") or told.get("renameto") or told.get("copyto")
            or (told.get("---") if "+++" in told else None)
            or (same.pop() if len(same) == 1 else pairs[0][1] if len(pairs) == 1 else None))


def commentable(diff):
    """{path: the new-side line numbers a review comment can go on} for
    every file the diff changes (an empty set: a file with no new side)."""
    files = {}
    for part in re.split(r"(?m)^(?=diff --git )", diff):
        lines = part.splitlines()
        if not lines or not lines[0].startswith("diff --git "):
            continue
        path = file_path(lines)
        if path is None:
            continue
        files[path] = set()
        new, in_hunk = 0, False
        for x in lines[1:]:
            if x.startswith("@@"):
                m = re.match(r"@@ -\d+(?:,\d+)? \+(\d+)", x)
                new, in_hunk = (int(m.group(1)) if m else 0), True
            elif in_hunk and x.startswith(("+", " ")):
                files[path].add(new)
                new += 1
    return files


def quiet(text):
    """Model text goes out under the user's login: no @-pings from it, and
    no markers."""
    return re.sub(r"@(?=[\w-])", "@​", str(text)).replace("<!--", "&lt;!--")


def post(repo, pr, r, answer, description):
    """The reviewer's answer as one review on the head, marked with the
    description it was shown (its fingerprint, or None if that's unknown).
    Returns "clean" or "findings". Raises ValueError if the answer isn't a review, and
    RuntimeError if GitHub didn't take it (the same answer can go next pass),
    C.Unavailable if GitHub didn't answer."""
    n, head = pr["number"], pr["headRefOid"]
    # Anything but a list of findings is no review at all, never a clean one.
    found = answer.get("findings") if isinstance(answer, dict) else None
    text = ("path", "title", "body")
    if (not isinstance(found, list) or not isinstance(answer.get("summary", ""), str)
            or not all(isinstance(f, dict) and all(isinstance(f.get(k), str) for k in text) for f in found)):
        raise ValueError(f"not a review: {json.dumps(answer)[:100]}")
    diff = C.gh("api", "-H", "Accept: application/vnd.github.diff",
                C.compare(repo, pr["baseRefName"], head))
    # Read wrong, every finding would land as a note and the review count as
    # clean.
    if not isinstance(diff, str) or not diff.startswith("diff --git "):
        raise RuntimeError("couldn't read the diff")
    files = commentable(diff)
    mark, who = finding(r), NAMES[r]
    comments, whole_file, loose = [], [], []
    for f in found:
        sev = f.get("severity") if f.get("severity") in ("P0", "P1", "P2") else "P1"
        msg = f"**{who} · {sev}** {quiet(f['title'])}\n\n{quiet(f['body'])}"
        if isinstance(f.get("covers"), str) and f["covers"].strip():
            msg += f"\n\n**A complete fix covers:** {quiet(f['covers'])}"
        lines = files.get(f["path"])
        if lines is None:
            # Not a file this PR changes (a caller it breaks, say): still a
            # finding, a thread on the PR's first changed file that says
            # where. With none to go on, it's listed in the review.
            on = next(iter(files), None)
            if on is None:
                loose.append(f"- `{quiet(f['path'])}`: " + msg.replace("\n\n", " "))
            else:
                whole_file.append(({"path": on, "body": f"{msg}\n\n(In `{quiet(f['path'])}`, which this PR doesn't change.)\n\n{mark}"},
                                   f"- `{quiet(f['path'])}`: " + msg.replace("\n\n", " ")))
            continue
        if not lines:
            # Changed, but with no new side to comment on (deleted, binary,
            # a mode change): a thread on the whole file, still a finding.
            whole_file.append(({"path": f["path"], "body": msg + "\n\n" + mark}, f"- `{quiet(f['path'])}`: " + msg.replace("\n\n", " ")))
            continue
        try:
            want = int(f.get("line"))
        except (TypeError, ValueError):
            want = min(lines)
        line = want if want in lines else min(lines, key=lambda x: abs(x - want))
        comments.append({"path": f["path"], "line": line, "side": "RIGHT", "body": msg + "\n\n" + mark})
    k = len(comments) + len(whole_file) + len(loose)
    body = (f"**{who} review** of {head[:7]}: "
            + (f"{k} finding{'s' * (k > 1)}. Fix or answer each, then resolve its thread." if k else "nothing found."))
    if answer.get("summary"):
        body += f"\n\n{quiet(answer['summary'])}"
    if loose:
        body += "\n\nFindings with no changed file to go on:\n" + "\n".join(loose)
    if whole_file:
        # In full here too: the threads below are separate calls, and a
        # finding mustn't hang on one of them.
        body += "\n\nFindings on whole files (each a thread too):\n" + "\n".join(text for _, text in whole_file)
    body += f"\n\n<!-- {r}-review:v1 " + json.dumps({"commit_sha": head, "base": pr["baseRefName"], "findings": k, "description": description}) + " -->"
    if not isinstance(C.write("api", "-X", "POST", f"repos/{repo}/pulls/{n}/reviews", "--input", "-",
                              input=json.dumps({"commit_id": head, "event": "COMMENT", "body": body, "comments": comments})), dict):
        raise RuntimeError("GitHub didn't take the review")
    # A review can't hold file-level comments: they go on their own, after
    # it, so a review GitHub turns away (posted again next pass) leaves none
    # behind. Its body has them already, so one GitHub won't take loses nothing.
    for c, _ in whole_file:
        if not isinstance(C.write("api", "-X", "POST", f"repos/{repo}/pulls/{n}/comments", "--input", "-",
                                  input=json.dumps({**c, "commit_id": head, "subject_type": "file"}), check=False), dict):
            log(f"{repo}#{n}: couldn't post {who}'s finding on {c['path']}")
    log(f"{repo}#{n}: {who}'s review posted, {k} finding(s)")
    return "findings" if k else "clean"
