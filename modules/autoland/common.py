"""What every part of the steward shares: its settings, gh, its state files,
and Claude jobs. Calls that touch the outside (gh, write, job, kick, token) are used
as `C.gh(...)` and so on, so a test can stand in for them in one place.

The steward talks to GitHub only as its GitHub App (config.app): the API
through gh and git both, each owner's repos with a token of the App's
installation on that owner. Whose repos a call is about is set around it
(`acting_for`); a call outside one fails rather than going as the machine
user's own gh login, which every Claude session on the machine shares.
`autoland-steward status` changes nothing, and reads as whoever runs it."""

import base64
import contextlib
import hashlib
import hmac
import json
import os
import re
import stat
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from datetime import datetime

import seal as S

CFG = json.load(open(os.environ["AUTOLAND_CONFIG"]))
CONTEXT = CFG["context"]
DRY = len(sys.argv) > 1 and sys.argv[1] == "status"
# A dry run keeps its own state, so its once-a-day reads (settings, branches)
# stay once a day instead of running every pass on the owner's API quota.
STATE = CFG["stateDir"] + ("/dry" if DRY else "")
NOW = time.time()
MIN = 60
DEP_RE = re.compile(r"^(renovate|dependabot)/")


def log(*a):
    print(*a, flush=True)


class Unavailable(RuntimeError):
    """GitHub didn't answer: a rate limit, a 5xx, no connection. Nothing the
    call asked for was wrong, so it counts against nobody: the same call goes
    again next pass."""


# What GitHub says of any rule on a private repo of a free account: the plan
# offers none there, so there are none to keep or to merge by.
NO_RULES = "Upgrade to GitHub Pro"


class Gone(RuntimeError):
    """GitHub has no such thing (404), or had it and deleted it (410): asking
    again won't bring it back."""


UNAVAILABLE = re.compile(r"rate limit|HTTP (429|5\d\d)|error connecting|connection (reset|refused)|timed? ?out|\bEOF\b|network is unreachable|no such host|TLS handshake", re.I)


# When the rate limit that turned one of this pass's calls away resets, once
# a call has met it: no pass of any repo before then could do anything.
LIMITED = []


def limited(stderr, env):
    """Note when the rate limit gh met resets. `gh api rate_limit` costs
    nothing against it; a limit with none spent (a secondary one) notes
    nothing. Each owner's installation has a budget of its own, and a pass
    that met one waits for it as if it were all of them."""
    if LIMITED or not re.search(r"rate limit", stderr, re.I):
        return
    p = subprocess.run(["gh", "api", "rate_limit", "--jq", "[.resources[] | select(.remaining == 0) | .reset] | max // 0"],
                       capture_output=True, text=True, env=env)
    try:
        LIMITED.append(float(p.stdout.strip()) if p.returncode == 0 else 0.0)
    except ValueError:
        LIMITED.append(0.0)


# git's credential, for a fetch from or a push to GitHub: gh's, so the
# App's token in gh_env(), and none of the machine user's helpers.
GIT_AUTH = ("-c", "credential.helper=", "-c", "credential.helper=!gh auth git-credential")
# GitHub's REST API, where the App's own calls (its installations, their
# tokens) go: gh can't make them, since it sends a JWT as a token, not as
# the bearer GitHub wants. A check points it at a stand-in.
API = "https://api.github.com"
# Whose repos the calls now are about, innermost last (acting_for).
_for = []
# The App's installations, by owner (lower case), and their tokens: each
# minted once a process and kept in memory only. The state dir is the
# machine user's, so a token written there would let anything of theirs act
# as the App for the rest of its hour.
_installs, _tokens = {}, {}


@contextlib.contextmanager
def acting_for(repo):
    """The GitHub calls inside are about the repos of repo's owner (OWNER or
    OWNER/NAME): they go as the App's installation on that owner."""
    _for.append(repo.split("/")[0].lower())
    try:
        yield
    finally:
        _for.pop()


def _b64(b):
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def app_jwt():
    """A JWT that says this is the App, good for nine minutes, signed with
    its key: systemd's credential app-key, which only the steward's passes
    are given. Python's standard library has no RSA, so openssl signs it."""
    now = int(time.time())
    signed = ".".join(_b64(json.dumps(x, separators=(",", ":")).encode()) for x in (
        {"alg": "RS256", "typ": "JWT"}, {"iat": now - 60, "exp": now + 540, "iss": str(CFG["app"]["id"])}))
    key = os.path.join(os.environ.get("CREDENTIALS_DIRECTORY", ""), "app-key")
    p = subprocess.run(["openssl", "dgst", "-sha256", "-sign", key, "-binary"], input=signed.encode(), capture_output=True)
    if p.returncode != 0:
        raise RuntimeError(f"couldn't sign as the App: {p.stderr.decode(errors='replace').strip()[:200]}")
    return f"{signed}.{_b64(p.stdout)}"


def app_call(method, path, jwt, body=None):
    """A call the App makes as itself, with body as its JSON; raises as gh()
    does."""
    data = json.dumps(body).encode() if body is not None else b"" if method == "POST" else None
    req = urllib.request.Request(f"{API}/{path}", method=method, data=data, headers={
        "Authorization": f"Bearer {jwt}", "Accept": "application/vnd.github+json",
        "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "burrow-autoland",
        **({"Content-Type": "application/json"} if body is not None else {})})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.load(r)
    except urllib.error.HTTPError as e:
        why = f"{method} {path}: HTTP {e.code} {e.read(300).decode(errors='replace')}"
        out = e.code in (429,) or e.code >= 500 or e.headers.get("x-ratelimit-remaining") == "0"
        raise (Unavailable if out else RuntimeError)(why) from None
    except OSError as e:  # no answer at all (URLError is one)
        raise Unavailable(f"{method} {path}: {e}") from None


def installations(jwt):
    """The App's installations, by owner (lower case), every page of them: a
    public App can be on accounts that aren't among the owners, and they
    take up pages too. A page GitHub didn't answer raises, so a lookup cut
    short is never read as an owner it isn't on."""
    found, page = {}, 1
    while True:
        got = app_call("GET", f"app/installations?per_page=100&page={page}", jwt)
        found.update({str((i.get("account") or {}).get("login", "")).lower(): i["id"] for i in got})
        if len(got) < 100:
            return found
        page += 1


def token(owner):
    """A token of the App's installation on owner (lower case). One lasts an
    hour and a pass twenty minutes at most, so each is minted once a
    process, and again only if it has under ten minutes left."""
    have = _tokens.get(owner)
    if have and have[1] - time.time() > 600:
        return have[0]
    jwt = app_jwt()
    if owner not in _installs:
        _installs.clear()
        _installs.update(installations(jwt))
    if owner not in _installs:
        raise RuntimeError(f"the App {CFG['app']['slug']} isn't installed on {owner}")
    got = app_call("POST", f"app/installations/{_installs[owner]}/access_tokens", jwt)
    _tokens[owner] = (got["token"], ts(got["expires_at"]))
    return got["token"]


def gh_env(repo=None):
    """The environment gh runs in, and git with gh as its only credential
    helper (GIT_AUTH): the App's token for repo's owner, else for the owner
    it's acting for. A status run only reads, as whoever runs it."""
    if DRY:
        return None
    if repo is None and not _for:
        raise RuntimeError("a GitHub call outside acting_for: it would go as the machine user's own login")
    return {**os.environ, "GH_TOKEN": token(repo.split("/")[0].lower() if repo else _for[-1])}


def gh(*args, input=None, check=True):
    """Run gh; return its stdout (parsed as JSON when it is JSON). Raises
    Unavailable when GitHub didn't answer, RuntimeError when it said no."""
    env = gh_env()
    p = subprocess.run(["gh", *args], input=input, capture_output=True, text=True, env=env)
    if p.returncode != 0:
        why = f"gh {' '.join(args[:3])}: {p.stderr.strip()[:300]}"
        limited(p.stderr, env)
        if check:
            raise (Unavailable if UNAVAILABLE.search(p.stderr) else RuntimeError)(why)
        log("  failed:", why)
        return None
    out = p.stdout.strip()
    try:
        return json.loads(out) if out else ""
    except ValueError:
        return out


def compare(repo, base, head):
    """The REST path of GitHub's compare of head with base, each a branch's
    name or a commit. Each goes in quoted whole, which GitHub decodes once:
    any name git allows (a `#`, `%`, `?` or slash in it) is the branch of
    exactly that name, never cut short or read as another's."""
    return f"repos/{repo}/compare/{urllib.parse.quote(base, safe='')}...{urllib.parse.quote(head, safe='')}"


def said(e):
    """What GitHub said, from gh()'s error ("gh ARGS: what gh printed"), on
    one line for a stuck PR's listing, without gh's own "gh: "; any other
    error's text as it is."""
    text = str(e)
    if text.startswith("gh "):
        text = text.partition(": ")[2].removeprefix("gh: ")
    return "; ".join(x.strip() for x in text.splitlines() if x.strip())


def fresh(path, etag=None):
    """GET path unless it's unchanged since GitHub gave it etag: (its etag,
    its JSON), or None when it's unchanged. GitHub doesn't count a request
    it answers 304 against the rate limit, so a look at something that
    rarely changes costs nothing most of the times it's taken. Raises as
    gh() does, and Gone when there's no such thing."""
    env = gh_env()
    p = subprocess.run(["gh", "api", "-i", *(["-H", f"If-None-Match: {etag}"] if etag else []), path],
                       capture_output=True, text=True, env=env)
    status, _, rest = p.stdout.partition("\n")
    code = status.split(" ")[1:2]
    if code == ["304"]:
        return None
    head, _, body = rest.partition("\n\n")  # text mode reads GitHub's \r\n as \n
    if p.returncode != 0:
        why = f"gh api {path}: {p.stderr.strip()[:300]}"
        limited(p.stderr, env)
        if code in (["404"], ["410"]):
            raise Gone(why)
        raise (Unavailable if UNAVAILABLE.search(p.stderr) else RuntimeError)(why)
    tag = next((h.split(":", 1)[1].strip() for h in head.split("\n") if h.lower().startswith("etag:")), None)
    return tag, json.loads(body)


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


def me():
    """The steward's login as REST gives it: its App's bot. The reviewers'
    reviews, its statuses, comments and merges are posted as it, and nobody
    else can post as it: a user's login can't have "[bot]" in it."""
    return CFG["app"]["slug"] + "[bot]"


def mine(actor):
    """Whether a GraphQL author is the steward. GraphQL gives a bot's login
    without "[bot]", and a user can hold that name as theirs, so only the
    App's Bot is."""
    return bool(actor) and actor.get("__typename") == "Bot" and actor.get("login") == CFG["app"]["slug"]


def ours(actor):
    """Whether a GraphQL author is one of the steward's authors: a person of
    `authors`, or one of `bots` as GitHub's own Bot, as `mine` reads one
    (the machinery's App, whose Claude jobs post and push, among them)."""
    if not actor:
        return False
    if actor.get("__typename") == "Bot":
        return actor.get("login") in CFG["bots"]
    return actor.get("login") in CFG["authors"]


def rest_ours(login):
    """The same for a login as REST gives it, which spells a bot SLUG[bot]:
    a name no person's can be."""
    if not login:
        return False
    if login.endswith("[bot]"):
        return login[: -len("[bot]")] in CFG["bots"]
    return login in CFG["authors"]


_key = []


def seal(repo, n, name, verdict, head, base):
    """What a verdict's marker carries after the head (seal.py), or None
    when there's no key to check it with. A judge's key is kept in the state
    dir, and only the job a task hands its seal to is told it. A major's is
    renovate-sweep's (majorKey), which its job seals with."""
    if name == S.MAJOR:
        k = CFG.get("majorKey") and S.key(CFG["majorKey"], create=not DRY)
        return S.major(k, repo, n, verdict, head, base) if k else None
    if not _key:
        # The real state dir's, dry run or not, so `status` reads the
        # verdicts the real passes' judges wrote; a dry run with none yet
        # writes nothing and makes do with one of its own.
        path = os.path.join(CFG["stateDir"], "verdict.key")
        if DRY and not os.path.exists(path):
            _key.append(os.urandom(32).hex())
            return seal(repo, n, name, verdict, head, base)
        key = S.key(path, create=not DRY)
        if key is None:
            raise RuntimeError(f"{path} isn't a key: the steward's next pass makes a new one")
        _key.append(key)
    return S.mac(_key[0], repo, n, name, verdict, head, base)


def quote(body):
    """A PR's description as the reviewers are shown it: each line quoted,
    so nothing in it reads as their brief going on."""
    return "\n".join(f"> {x}".rstrip() for x in body.strip().splitlines()) or "(none)"


def fingerprint(body):
    """What a review or a judge's verdict carries of the description it was
    on: the reasons a description gives are part of what's judged."""
    return hashlib.sha256(quote(body).encode()).hexdigest()[:16]


def marker(pr, name, head, repo=None, base=None, on=None):
    """The verdict in the newest `<!-- NAME: VERDICT SHA -->` on the PR for
    exactly this head, from one of its authors (`ours`: whoever its Claude
    jobs, the judges among them, post as) only; else None. With
    repo, only a marker that also carries its seal, for this head on the PR's
    base as it is now (or on `on`, what else the verdict is on), counts; with
    base, only one that names that base (`<!-- NAME: VERDICT SHA on BASE -->`)."""
    pat = re.compile(rf"<!-- {re.escape(name)}: ([a-z]+) ([0-9a-f]{{40}})(?: on (\S+))?(?: ([0-9a-f]{{32}}))? -->")
    verdict = None
    for c in pr["comments"]["nodes"]:  # oldest first
        if not ours(c["author"]):
            continue
        for m in pat.finditer(c["body"] or ""):
            if m.group(2) != head or (base is not None and m.group(3) != base):
                continue
            want = repo and seal(repo, pr["number"], name, m.group(1), head, on or pr["baseRefName"])
            if repo and not (want and hmac.compare_digest(m.group(4) or "", want)):
                continue
            verdict = m.group(1)
    return verdict


def ts(s):
    return datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp() if s else 0


# When each repo's PRs next need a pass, as this pass worked out: a wait that
# ends then (a new head's settle delay, a reviewer's timeout, the time left
# before a Claude job takes its findings). Saved to wake.json, where
# burrow-events (crates/burrow-events) reads it and starts that repo's pass
# then.
WAKE = {}


def wake(repo, at):
    """Ask for a pass of repo at `at`, if that's still to come (one a rate
    limit turned away waits for its reset: steward.after_limit)."""
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
    poke(f"{CFG['kick']}/{repo}")


def poke(url):
    """Start the pass a burrow-events kick URL names, now: this steward's for
    a repo, or another handler's (the watchdog's, when what it waits on
    changed). Best effort, like kick."""
    if DRY:
        return
    try:
        urllib.request.urlopen(urllib.request.Request(url, data=b"", method="POST"), timeout=5).close()
    except OSError as e:
        log(f"couldn't kick {url}: {e}")


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


def acted_on(pr):
    """Whether the steward does anything with the PR beyond saying it's not
    its to do: not a draft, and from one of its authors."""
    return not pr["isDraft"] and not pr["isCrossRepository"] and ours(pr["author"])


DEQUEUE = "mutation($id: ID!) { dequeuePullRequest(input: {id: $id}) { clientMutationId } }"


def queued(pr):
    """Whether a merge waits on it for GitHub to make later: auto-merge, or
    a place in a merge queue. Either can be there without the other."""
    return bool(pr["autoMergeRequest"] or pr["mergeQueueEntry"])


def unland(repo, pr):
    """Take off every merge waiting on it, so GitHub merges nothing later:
    its auto-merge, and its place in a merge queue, which turning auto-merge
    off leaves where it is. False if GitHub didn't take one off."""
    off = not pr["autoMergeRequest"] or write(
        "pr", "merge", str(pr["number"]), "-R", repo, "--disable-auto", check=False) is not None
    out = not pr["mergeQueueEntry"] or write(
        "api", "graphql", "-f", f"query={DEQUEUE}", "-f", f"id={pr['id']}", check=False) is not None
    return off and out


def repo_cfg(repo):
    """The defaults, with this repo's overrides from config.repos on top."""
    c = dict(CFG["defaults"])
    c.update(CFG["repos"].get(repo, {}))
    return c


def projects(strict=False):
    """(folder, repo) for each project in projectsDir, by its origin URL.
    Only a clone of its own counts: a hidden folder, or a worktree of the
    repo (`.git` is a file there) checked out on some other branch, sorts
    first and shares the origin, but a job sent there works on the wrong
    tree. A hidden folder named for the repo itself is that repo's clone
    (FieldmouseWorks/.github in .github). Strict, what can't be read
    (projectsDir, a folder's `.git`, its origin, other than there being
    none) raises rather than being left out: for repos.restart, which must
    see every clone a repo has."""
    src = CFG["projectsDir"]
    try:
        names = sorted(os.listdir(src))
    except FileNotFoundError:
        names = []
    except OSError:
        if strict:
            raise
        names = []
    for d in names:
        try:
            if not stat.S_ISDIR(os.stat(os.path.join(src, d, ".git")).st_mode):
                continue
        except (FileNotFoundError, NotADirectoryError):
            continue
        except OSError:
            if strict:
                raise
            continue
        p = subprocess.run(["git", "-C", os.path.join(src, d), "remote", "get-url", "origin"],
                           capture_output=True, text=True)
        if strict and p.returncode and "No such remote" not in p.stderr:
            raise RuntimeError(f"{d}: git can't read its origin: {p.stderr.strip() or f'exit {p.returncode}'}")
        url = github_repo(p.stdout.strip())
        if url and (not d.startswith(".") or d.lower() == url.split("/")[1].lower()):
            yield d, url


def github_repo(url):
    """OWNER/REPO of a GitHub remote URL, or None."""
    url = re.sub(r"^(https://github\.com/|git@github\.com:)", "", url).removesuffix(".git")
    return url if url.count("/") == 1 else None


def project_dir(repo):
    """The repo's checkout in projectsDir, or None."""
    return next((d for d, url in projects() if url.lower() == repo.lower()), None)


def rank(repo):
    """Where repo's reviews go in line, first to last: 0 for a lead repo
    (`leadRepos`, what the pipeline itself is built from), ahead of
    everything; 1 for another whose PRs work on this pipeline itself
    (`pipelineRepos`), so everything after them is quicker; 2 for the rest.
    proj ranks their Claude jobs the same way, by the clone's origin."""
    r = repo.lower() if isinstance(repo, str) else None
    if r in {x.lower() for x in CFG["leadRepos"]}:
        return 0
    return 1 if r in {x.lower() for x in CFG["pipelineRepos"]} else 2


def job(repo, task, clone=False, pipeline=True, timeout=None):
    """Hand a task to a Claude job in the repo's project. One that settles a
    PR or the branch PRs land on is pipeline work (proj run --pipeline): it
    goes ahead of the line, and in the pipeline's own repos (pipelineRepos)
    in a slot kept for it; proj decides which. Its turns stop after timeout
    minutes (jobTimeout's by default). Returns the job id."""
    d = project_dir(repo)
    if d is None and clone and not DRY:
        d = repo.split("/")[1]
        # The project's clone is the machine user's, for its jobs to push
        # from as them: cloned as them, not as the App.
        subprocess.run(["gh", "repo", "clone", repo, os.path.join(CFG["projectsDir"], d)], check=False)
        d = project_dir(repo)
    if d is None:
        return None
    if DRY:
        log(f"  would: proj run {d} ...")
        return "dry"
    # Each of its turns starts the repo's pass when it takes its slot (it
    # sees the job at work, not queued) and when it ends (the pass reads what
    # it did and acts on it, so --owned: the turn needn't say what comes
    # next); one that goes quiet for jobIdle, or runs past jobTimeout, is
    # stopped, and ends then. It's the machine's own work, so it acts on
    # GitHub as the machinery's App, where there is one.
    app = ["--app", CFG["machinery"]] if CFG.get("machinery") else []
    p = subprocess.run([CFG["proj"], "run", *(["--pipeline"] if pipeline else []), *app,
                        "--timeout", f"{timeout or CFG['jobTimeout']}m",
                        "--idle", f"{CFG['jobIdle']}m", "--kick", f"autoland/{repo}", "--owned", d, task],
                       capture_output=True, text=True)
    return p.stdout.strip() or None


def job_state(jid):
    """queued, running, or how it ended (done, error, failed, timed out ...);
    None when proj can't say."""
    if DRY:
        return "running"
    p = subprocess.run([CFG["proj"], "state", jid], capture_output=True, text=True)
    return p.stdout.strip() or None


def jobs():
    """(id, project, the repos it's for) of every job proj ever ran, from
    its own record (jobsDir): `proj run` writes a job's project folder to
    its `project` file as it starts it, then its kicks to `kick`, where
    the steward's jobs and proj-inbox's name their repo
    (HANDLER/OWNER/REPO, lowercased here). One still being made, with no
    project yet, is left out: it can only start in its folder as that is
    by then. Raises when any of the record can't be read: none of it is
    taken for missing but what isn't there."""
    root = CFG["jobsDir"]
    try:
        ids = sorted(os.listdir(root))
    except FileNotFoundError:
        return []
    found = []
    for jid in ids:
        d = os.path.join(root, jid)
        if jid.startswith("."):  # proj's own (.waits.json, .slots)
            continue
        try:
            if not stat.S_ISDIR(os.stat(d).st_mode):
                continue
        except FileNotFoundError:
            continue
        try:
            with open(os.path.join(d, "project")) as f:
                project = f.read().removesuffix("\n")
        except FileNotFoundError:
            continue
        try:
            with open(os.path.join(d, "kick")) as f:
                kicks = f.read().splitlines()
        except FileNotFoundError:
            kicks = []
        found.append((jid, os.path.normpath(project), {x.split("/", 1)[1].lower() for x in kicks if x.count("/") == 2}))
    return found


def stop_job(jid):
    """End a Claude job (proj stop): its turn, queued or running, and
    anything it awaits. True once it's over, one proj no longer has
    included; False if proj couldn't stop it, and it may still be at work."""
    if DRY:
        log(f"  would: proj stop {jid}")
        return True
    p = subprocess.run([CFG["proj"], "stop", jid], capture_output=True, text=True)
    if p.returncode and p.stderr.startswith(f"no job {jid}"):
        return True
    if p.returncode:
        log(f"proj stop {jid}: {p.stderr.strip()[-300:]}")
    return p.returncode == 0


def forget_issues(repo):
    """proj-inbox forgets the repo's issues (`proj-inbox forget`), once a
    pass of its under way has ended: which job each is, what of it it has
    answered. Nothing to forget where there's no inbox; a refusal raises."""
    if DRY or not CFG["inbox"]["enable"]:
        return
    p = subprocess.run([CFG["inbox"]["command"], "forget", repo], capture_output=True, text=True)
    if p.returncode:
        raise RuntimeError(f"proj-inbox forget {repo}: {p.stderr.strip() or f'exit {p.returncode}'}")


def coverage(repos, covered):
    """repos.json, for renovate-sweep, and when: every repo the steward has,
    with how its proofs run and whether this pass saw to its PRs."""
    done = {r["repo"] for r in covered}
    return {"at": NOW, "repos": {r["repo"]: {"proofJobs": repo_cfg(r["repo"])["proofJobs"], "covered": r["repo"] in done}
                                 for r in repos}}


def after_limit(kick):
    """GitHub's rate limit turned the pass away somewhere, finding its repos,
    in its PRs or after (the hooks, the owner's issue): it comes back whole
    at the reset, a full pass as one (its settings, hooks and issue too:
    burrow-events takes "" for it), a kicked one for its repos, whatever
    they had booked. Every other wake-up waits for the reset too, the ones
    booked before it was met and other passes'. None sooner could do
    anything, and none then waits on another event or the hourly pass."""
    at = LIMITED[0] + 1
    again = [""] if kick is None else kick
    wakes = {k: max(v, at) for k, v in load("wake.json", {}).items()
             if v > NOW and k.lower() not in {r.lower() for r in again}}
    save("wake.json", {**wakes, **dict.fromkeys(again, at)})
