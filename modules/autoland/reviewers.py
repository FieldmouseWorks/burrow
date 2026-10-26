"""The steward's reviewers: Codex (the codex CLI) and DeepSeek (DeepSeek
Harness), each an agent that reads a snapshot of the PR and answers with its
findings.

A pass takes the snapshot and queues the review (`queue`); the reviewers'
service (review_worker.py) runs the agents as a user of their own, with no
GitHub access and no secrets paths in what they read; the next pass posts
what came back (`post`), under the steward's login and marked as that
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

import common as C
from common import CFG, STATE, load, log, save, ts

NAMES = {"codex": "Codex", "deepseek": "DeepSeek"}
# Under STATE: what's asked for, the snapshot each one reads, what came back,
# and a clone per repo the snapshots are taken from.
QUEUE, SNAP, DONE, GIT = "review/queue", "review/snap", "review/done", "review/git"
# What the reviewers' accounts have left, as their service last read it.
ACCOUNT = "review/account"
ORIGIN = "https://github.com/{}.git"
OUT = "reviewers.json"  # a reviewer out of service, and until when
BALANCE = f"{ACCOUNT}/deepseek.json"


def on(repo):
    """The reviewers this repo has, in a fixed order."""
    rc = C.repo_cfg(repo)
    return [r for r in NAMES if CFG["reviewers"][r]["enable"] and rc[r]]


def out(r):
    """Why the reviewer is out of service (usage spent, not logged in), or
    None. Out, it isn't asked and isn't waited for."""
    o = load(OUT, {}).get(r)
    return o["why"] if o and C.NOW < o["until"] else None


def low_balance():
    """The owner's line when DeepSeek's account, as its last review left it,
    is down to under `lowBalance` (it stops reviewing at nothing), or none."""
    rv = CFG["reviewers"]["deepseek"]
    left = load(BALANCE, {}).get("left") or {}
    if not rv["enable"] or not left or any(v >= rv["lowBalance"] for v in left.values()):
        return []
    said = " and ".join(f"{v:.2f} {c}" for c, v in sorted(left.items()))
    return [("DeepSeek's balance is running low", "DeepSeek",
             f"{said} left. Top it up: once it's spent, DeepSeek is out of service and PRs merge on the other reviewers' word")]


def review_re(r):
    return re.compile(rf"<!-- {r}-review:v1 (\{{.*?\}}) -->")


def finding(r):
    return f"<!-- {r}-finding -->"


def verdict(pr, head, r):
    """What the reviewer last said about this exact head on the PR's base
    (clean, findings or none), when, and how many of its reviews on the PR had findings. Only
    reviews the steward posted count: anyone can write the marker."""
    pat = review_re(r)
    v, at, rounds = "none", 0, 0
    for x in pr["reviews"]["nodes"]:
        m = pat.search(x["body"] or "")
        if not m or (x["author"] or {}).get("login") != C.me():
            continue
        try:
            said = json.loads(m.group(1))
            found = int(said.get("findings", 0)) > 0
        except (ValueError, TypeError, AttributeError):
            continue
        rounds += found
        # Retargeted, the PR is another change on the same head: what was
        # said about it on its old base doesn't count.
        if (said.get("commit_sha") == head and said.get("base") == pr["baseRefName"]
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
    as `rev` (the base branch) has them: a PR can't rewrite its own rules.
    An AGENTS.md in a secrets path is never read: its rules would reach the
    reviewers in the prompt."""
    have = {f for f in names(git("-C", mirror, "ls-tree", "-r", "-z", "--name-only", rev)) if not hidden(repo, f)}
    wanted = set()
    for p in paths:
        d = p
        while d:
            d = d.rpartition("/")[0]
            f = f"{d}/AGENTS.md" if d else "AGENTS.md"
            if f in have:
                wanted.add(f)
                break
    out = []
    for f in sorted(wanted):
        text = git("-C", mirror, "show", f"{rev}:{f}").decode(errors="replace")
        m = re.search(r"^## Code Review Rules\s*$(.*?)(?=^## |\Z)", text, re.M | re.S)
        if m:
            out.append(f"From {f}:\n{m.group(1).strip()}")
    return "\n\n".join(out)


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
    if meta:
        return name, meta
    mirror = os.path.join(STATE, GIT, repo.replace("/", "__") + ".git")
    if not os.path.isdir(mirror):
        git("init", "-q", "--bare", mirror)
    git("-C", mirror, "-c", "credential.helper=", "-c", "credential.helper=!gh auth git-credential",
        "fetch", "-q", "--no-tags", ORIGIN.format(repo),
        f"+refs/pull/{n}/head:refs/pr/{n}", f"+refs/heads/{base}:refs/base/{base}")
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
    dst = os.path.join(STATE, SNAP, name)
    shutil.rmtree(dst, ignore_errors=True)
    # Nothing from the steward's own git config (an LFS filter, say) runs
    # on what the PR put there.
    bare = {**os.environ, "GIT_CONFIG_GLOBAL": "/dev/null", "GIT_CONFIG_NOSYSTEM": "1"}
    git("init", "-q", dst, env=bare)
    git("-C", dst, "fetch", "-q", "--no-tags", mirror, f"refs/snap/{name}:refs/heads/review", env=bare)
    git("-C", mirror, "update-ref", "-d", f"refs/snap/{name}")
    git("-C", dst, "checkout", "-q", "review", env=bare)
    meta = {"rules": rules(repo, mirror, f"refs/base/{base}", [p for p in changed if not (hidden(repo, p) or p in moved)]),
            "secret": [shown(p) for p in changed if hidden(repo, p) or p in moved]}
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
                   and (x["user"] in CFG["authors"] or x["user"] == C.me())]
        at = c.get("line") or c.get("original_line")
        text.append(f"- {c['path']}{f':{at}' if at else ''}: {c['body'].replace(mark, '').strip()}\n"
                    + "".join(f"  Reply from {x['user']}: {x['body'].strip()}\n" for x in replies))
    return "\n".join(text)


def item_name(key, r, head, base):
    return f"{r}--{snap_name(key, head, base)}"


def queue(repo, pr, key, r):
    """Queue the reviewer on the head's snapshot. Raises if the snapshot
    can't be taken (the head moved on, GitHub's unreachable)."""
    snap, meta = snapshot(repo, pr, key)
    save(f"{QUEUE}/{r}--{snap}.json", {
        "reviewer": r, "snap": snap, "repo": repo, "number": pr["number"], "headRefOid": pr["headRefOid"],
        "baseRefName": pr["baseRefName"], "title": pr["title"], "rules": meta["rules"], "secret": meta["secret"],
        "prior": prior(repo, pr["number"], r), "at": C.NOW})


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


def answered(key, r):
    """The names of the reviewer's answers, for any head, to the PR whose
    state file is `key`."""
    pre = f"{r}--{key[:-5]}-"
    return sorted(x.removesuffix(".json") for x in os.listdir(os.path.join(STATE, DONE)) if x.startswith(pre))


def taken(path):
    """Takes the queued review at path off the queue unless the reviewers'
    service is on it (it holds the entry's lock while it is: review_worker
    `review`). False if it is."""
    try:
        f = open(path)
    except OSError:
        return True
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
    An open PR's earlier head keeps a look the reviewers' service is on, the
    snapshot it reads, and what came back, which the PR's next pass posts
    on the commit it read (steward.py's `overtaken`)."""
    prs = {s.rsplit("-", 2)[0] for s in live}
    keep = set()
    for d in (QUEUE, DONE, SNAP):  # the queue first: a look still running keeps its snapshot
        for x in os.listdir(os.path.join(STATE, d)):
            snap = x.removesuffix(".json")
            if d != SNAP:
                snap = snap.partition("--")[2]
            if snap.rsplit("-", 3)[0].removeprefix("pr-") not in repos or snap in live:
                continue
            ours = snap.rsplit("-", 2)[0] in prs
            if d == QUEUE and ours:
                if not taken(os.path.join(STATE, d, x)):
                    keep.add(snap)
            elif not (ours and (d == DONE or snap in keep)):
                remove(os.path.join(STATE, d, x))


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


def post(repo, pr, r, answer):
    """The reviewer's answer as one review on the head. Returns "clean" or
    "findings". Raises ValueError if the answer isn't a review, and
    RuntimeError if GitHub didn't take it (the same answer can go next pass)."""
    n, head = pr["number"], pr["headRefOid"]
    # Anything but a list of findings is no review at all, never a clean one.
    found = answer.get("findings") if isinstance(answer, dict) else None
    text = ("path", "title", "body")
    if (not isinstance(found, list) or not isinstance(answer.get("summary", ""), str)
            or not all(isinstance(f, dict) and all(isinstance(f.get(k), str) for k in text) for f in found)):
        raise ValueError(f"not a review: {json.dumps(answer)[:100]}")
    diff = C.gh("api", "-H", "Accept: application/vnd.github.diff",
                f"repos/{repo}/compare/{pr['baseRefName']}...{head}", check=False)
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
        lines = files.get(f["path"])
        if lines is None:
            # Not a file this PR changes (a caller it breaks, say): still a
            # finding, a thread on the PR's first changed file that says
            # where. With none to go on, it's listed in the review.
            on = next(iter(files), None)
            if on is None:
                loose.append(f"- `{quiet(f['path'])}`: " + msg.replace("\n\n", " "))
            else:
                whole_file.append({"path": on, "body": f"{msg}\n\n(In `{quiet(f['path'])}`, which this PR doesn't change.)\n\n{mark}"})
            continue
        if not lines:
            # Changed, but with no new side to comment on (deleted, binary,
            # a mode change): a thread on the whole file, still a finding.
            whole_file.append({"path": f["path"], "body": msg + "\n\n" + mark})
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
    body += f"\n\n<!-- {r}-review:v1 " + json.dumps({"commit_sha": head, "base": pr["baseRefName"], "findings": k}) + " -->"
    if not isinstance(C.write("api", "-X", "POST", f"repos/{repo}/pulls/{n}/reviews", "--input", "-",
                              input=json.dumps({"commit_id": head, "event": "COMMENT", "body": body, "comments": comments}),
                              check=False), dict):
        raise RuntimeError("GitHub didn't take the review")
    # A review can't hold file-level comments: they go on their own. The
    # review already counts them, so a lost one still keeps the PR unsettled.
    for c in whole_file:
        if not isinstance(C.write("api", "-X", "POST", f"repos/{repo}/pulls/{n}/comments", "--input", "-",
                                  input=json.dumps({**c, "commit_id": head, "subject_type": "file"}), check=False), dict):
            log(f"{repo}#{n}: couldn't post {who}'s finding on {c['path']}")
    log(f"{repo}#{n}: {who}'s review posted, {k} finding(s)")
    return "findings" if k else "clean"
