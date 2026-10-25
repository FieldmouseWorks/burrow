"""What every part of the steward shares: its settings, gh, its state files,
and Claude jobs. Calls that touch the outside (gh, write, job, kick, me) are used
as `C.gh(...)` and so on, so a test can stand in for them in one place."""

import hashlib
import hmac
import json
import os
import re
import subprocess
import sys
import time
import urllib.request
from datetime import datetime

CFG = json.load(open(os.environ["AUTOLAND_CONFIG"]))
CONTEXT = CFG["context"]
CODEX = CFG["codexLogin"]
DS = CFG.get("deepseek") or {"enable": False}
DRY = len(sys.argv) > 1 and sys.argv[1] == "status"
# A dry run keeps its own state, so its once-a-day reads (settings, branches)
# stay once a day instead of running every pass on the owner's API quota.
STATE = CFG["stateDir"] + ("/dry" if DRY else "")
NOW = time.time()
MIN = 60
DEP_RE = re.compile(r"^(renovate|dependabot)/")


def log(*a):
    print(*a, flush=True)


def gh(*args, input=None, check=True):
    """Run gh; return its stdout (parsed as JSON when it is JSON)."""
    p = subprocess.run(["gh", *args], input=input, capture_output=True, text=True)
    if p.returncode != 0:
        why = f"gh {' '.join(args[:3])}: {p.stderr.strip()[:300]}"
        if check:
            raise RuntimeError(why)
        log("  failed:", why)
        return None
    out = p.stdout.strip()
    try:
        return json.loads(out) if out else ""
    except ValueError:
        return out


def write(*args, **kw):
    """A gh call that changes something: skipped (and logged) in status mode."""
    if DRY:
        log("  would:", "gh", " ".join(a if len(a) < 80 else a[:77] + "..." for a in args))
        return None
    return gh(*args, **kw)


def graphql(query, **variables):
    args = ["api", "graphql", "-f", f"query={query}"]
    for k, v in variables.items():
        if v is not None:
            args += ["-F" if isinstance(v, int) else "-f", f"{k}={v}"]
    return gh(*args)["data"]


def comment(repo, n, body):
    return write("api", f"repos/{repo}/issues/{n}/comments", "-f", f"body={body}", check=False)


_me = []


def me():
    """The login the steward runs as, read once a pass. Its Claude jobs and
    DeepSeek's reviews post as it too, and nobody else can."""
    if not _me:
        _me.append(gh("api", "user", "--jq", ".login"))
    return _me[0]


_key = []
KEY_RE = re.compile(r"^[0-9a-f]{64}$")


def seal(repo, n, name, verdict, head, base):
    """What a verdict's marker carries after the head: a MAC under a key kept
    in the state dir, which only the job a task hands it to is told. Every
    Claude job posts as the steward's login, so without it a job quoting PR
    text that holds a well-formed marker would pass for the verdict. It binds
    the base too: a PR retargeted onto another branch has another diff, which
    its verdict wasn't about."""
    if not _key:
        # The real state dir's, dry run or not, so `status` reads the
        # verdicts the real passes' judges wrote; a dry run with none yet
        # writes nothing and makes do with one of its own.
        path = os.path.join(CFG["stateDir"], "verdict.key")
        if DRY and not os.path.exists(path):
            _key.append(os.urandom(32).hex())
            return seal(repo, n, name, verdict, head, base)
        try:
            key = open(path).read().strip()
        except FileNotFoundError:
            key = None
        if not KEY_RE.match(key or ""):
            if DRY:
                raise RuntimeError(f"{path} isn't a key: the steward's next pass makes a new one")
            # Missing, or cut short (a crash or a full disk as it was made):
            # an empty or short key is one anyone could seal with, so a new
            # one, whole or not at all. Verdicts sealed with a damaged one
            # stop counting, and their judges are asked again.
            if key is not None:
                log(f"{path} isn't a key; making a new one")
            tmp = f"{path}.{os.getpid()}.new"
            fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
            with os.fdopen(fd, "w") as f:
                f.write(os.urandom(32).hex())
                f.flush()
                os.fsync(f.fileno())
            if key is None:
                # Linked in whole, and only if no one made one meanwhile.
                try:
                    os.link(tmp, path)
                except FileExistsError:
                    pass
                os.remove(tmp)
            else:
                os.replace(tmp, path)
            key = open(path).read().strip()
            if not KEY_RE.match(key):
                raise RuntimeError(f"{path} couldn't be written")
        _key.append(key)
    msg = f"{repo}#{n} {name} {verdict} {head} {base}".encode()
    return hmac.new(_key[0].encode(), msg, hashlib.sha256).hexdigest()[:32]


def marker(pr, name, head, repo=None, base=None):
    """The verdict in the newest `<!-- NAME: VERDICT SHA -->` on the PR for
    exactly this head, from the steward's own login only; else None. With
    repo, only a marker that also carries its seal, for this head on the PR's
    base as it is now, counts; with base, only one that names that base
    (`<!-- NAME: VERDICT SHA on BASE -->`)."""
    pat = re.compile(rf"<!-- {re.escape(name)}: ([a-z]+) ([0-9a-f]{{40}})(?: on (\S+))?(?: ([0-9a-f]{{32}}))? -->")
    verdict = None
    for c in pr["comments"]["nodes"]:  # oldest first
        if (c["author"] or {}).get("login") != me():
            continue
        for m in pat.finditer(c["body"] or ""):
            if m.group(2) != head or (base is not None and m.group(3) != base):
                continue
            if repo and not hmac.compare_digest(m.group(4) or "", seal(repo, pr["number"], name, m.group(1), head, pr["baseRefName"])):
                continue
            verdict = m.group(1)
    return verdict


def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp() if s else 0


# When each repo's PRs next need a pass, as this pass worked out: a wait that
# ends then (a new head's settle delay, a reviewer's timeout, the time left
# before a Claude job takes its findings). Saved to wake.json, where
# burrow-events (../events/events.py) reads it and starts that repo's pass
# then.
WAKE = {}


def wake(repo, at):
    """Ask for a pass of repo at `at`, if that's still to come."""
    if at > NOW:
        WAKE[repo] = min(WAKE.get(repo, at), at)


def webhook_secret():
    """The secret GitHub signs the webhook's deliveries with (burrow-events
    checks them; repos.py gives it to each repo's hook)."""
    with open(os.path.join(os.environ.get("CREDENTIALS_DIRECTORY", ""), "webhook")) as f:
        return f.read().strip()


def kick(repo):
    """Start the repo's pass now (burrow-events), for something it would find
    that GitHub sends no event for. Best effort: the hourly catch-up is there
    for one that doesn't get through."""
    if DRY:
        return
    try:
        urllib.request.urlopen(urllib.request.Request(f"{CFG['kick']}/{repo}", data=b"", method="POST"), timeout=5).close()
    except OSError as e:
        log(f"{repo}: couldn't kick its pass: {e}")


def load(name, default):
    try:
        return json.load(open(os.path.join(STATE, name)))
    except (OSError, ValueError):
        return default


def save(name, value):
    path = os.path.join(STATE, name)
    try:
        with open(path + ".tmp", "w") as f:
            json.dump(value, f, indent=1)
        os.replace(path + ".tmp", path)
    except OSError:
        if not DRY:  # `status` from a shell that can't write the state dir: fine
            raise


def repo_cfg(repo):
    """The defaults, with this repo's overrides from config.repos on top."""
    c = dict(CFG["defaults"])
    c.update(CFG["repos"].get(repo, {}))
    return c


def project_dir(repo):
    """The repo's checkout in projectsDir (by origin URL), or None. Only a
    clone of its own counts: a hidden folder, or a worktree of the repo
    (`.git` is a file there) checked out on some other branch, sorts first
    and shares the origin, but a job sent there works on the wrong tree."""
    src = CFG["projectsDir"]
    for d in sorted(os.listdir(src)) if os.path.isdir(src) else []:
        if d.startswith(".") or not os.path.isdir(os.path.join(src, d, ".git")):
            continue
        p = subprocess.run(["git", "-C", os.path.join(src, d), "remote", "get-url", "origin"],
                           capture_output=True, text=True)
        url = re.sub(r"^(https://github\.com/|git@github\.com:)", "", p.stdout.strip()).removesuffix(".git")
        if url.lower() == repo.lower():
            return d
    return None


def job(repo, task, clone=False):
    """Hand a task to a Claude job in the repo's project. Returns the job id."""
    d = project_dir(repo)
    if d is None and clone and not DRY:
        d = repo.split("/")[1]
        subprocess.run(["gh", "repo", "clone", repo, os.path.join(CFG["projectsDir"], d)], check=False)
        d = project_dir(repo)
    if d is None:
        return None
    if DRY:
        log(f"  would: proj run {d} ...")
        return "dry"
    # Each of its turns that ends starts the repo's pass, which reads what it did.
    p = subprocess.run([CFG["proj"], "run", "--kick", f"autoland/{repo}", d, task], capture_output=True, text=True)
    return p.stdout.strip() or None


def job_state(jid):
    """queued, running, or how it ended (done, error, failed, timed out ...);
    None when proj can't say."""
    if DRY:
        return "running"
    p = subprocess.run([CFG["proj"], "state", jid], capture_output=True, text=True)
    return p.stdout.strip() or None
