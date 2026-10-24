"""What every part of the steward shares: its settings, gh, its state files,
and Claude jobs. Calls that touch the outside (gh, write, job, me) are used
as `C.gh(...)` and so on, so a test can stand in for them in one place."""

import json
import os
import re
import subprocess
import sys
import time
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


def marker(pr, name, head):
    """The verdict in the newest `<!-- NAME: VERDICT SHA -->` on the PR for
    exactly this head, from the steward's own login only; else None."""
    pat = re.compile(rf"<!-- {re.escape(name)}: ([a-z]+) ([0-9a-f]{{40}}) -->")
    verdict = None
    for c in pr["comments"]["nodes"]:  # oldest first
        if (c["author"] or {}).get("login") != me():
            continue
        for m in pat.finditer(c["body"] or ""):
            if m.group(2) == head:
                verdict = m.group(1)
    return verdict


def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp() if s else 0


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
    p = subprocess.run([CFG["proj"], "run", d, task], capture_output=True, text=True)
    return p.stdout.strip() or None


def job_state(jid):
    """queued, running, or how it ended (done, error, failed, timed out ...);
    None when proj can't say."""
    if DRY:
        return "running"
    p = subprocess.run([CFG["proj"], "state", jid], capture_output=True, text=True)
    return p.stdout.strip() or None
