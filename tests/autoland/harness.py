"""What the steward's checks share (check.py runs them): its config and
state in a temp dir; stand-ins for gh, Claude jobs, GitHub's git remote and
Conary's `cn` (each records how it was run, then does what it's told) and
burrow-events' kick port; the reviewers' service, with its own stand-ins
for the agent and its sandbox; and PRs shaped like its query's."""

import http.server
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse

tmp = tempfile.mkdtemp()

# burrow-events' kick port: which repos' passes were kicked.
KICKS = []


class Kicks(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get("Content-Length") or 0))
        KICKS.append(self.path)
        self.send_response(202)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def log_message(self, *a):
        pass


kicks = http.server.HTTPServer(("127.0.0.1", 0), Kicks)
threading.Thread(target=kicks.serve_forever, daemon=True).start()

os.makedirs(f"{tmp}/state")
os.makedirs(f"{tmp}/cred")
os.makedirs(f"{tmp}/home")
with open(f"{tmp}/cred/webhook", "w") as f:
    f.write("hook-secret\n")

# The reviewers' service, burrow-review (argv[3]), with its own tests'
# stand-ins for codex and bubblewrap (argv[4], crates/burrow-review/tests/fakes):
# codex records how it was run and does what $HOME/.fake/codex.json says.
REVIEW, FAKES = sys.argv[3], sys.argv[4]
os.makedirs(f"{tmp}/agents")
os.makedirs(f"{tmp}/home/.fake")
for fake, at in (("codex", f"{tmp}/agents/codex"), ("bwrap", f"{tmp}/bwrap")):
    with open(f"{FAKES}/{fake}.bash") as src, open(at, "w") as f:
        f.write(f"#!{shutil.which('bash')}\n" + src.read())
    os.chmod(at, 0o755)
with open(f"{tmp}/store-paths", "w") as f:
    f.write("/nix/store/aaaa-review-agents\n")


def review_service():
    """`autoland-review`: burrow-review works through the queue, as the
    module runs it, on the steward's state here."""
    with open(f"{tmp}/review.json", "w") as f:
        json.dump({"queue": f"{tmp}/state/review/queue", "done": f"{tmp}/state/review/done",
                   "sessions": f"{tmp}/state/review/sessions", "snapshots": f"{tmp}/trees",
                   "reviewers": {"codex": {"enable": True, "effort": "high", "parallel": 2, "off": []}}, "timeoutMinutes": 5,
                   "agents": f"{tmp}/agents", "sandbox": f"{tmp}/bwrap",
                   "box": {"store": f"{tmp}/store-paths", "caBundle": f"{tmp}/ca.crt", "links": {}},
                   "leadRepos": C.CFG["leadRepos"], "pipelineRepos": C.CFG["pipelineRepos"], "kick": C.CFG["kick"]}, f)
    subprocess.run([REVIEW, "work"], check=True, env={"REVIEW_CONFIG": f"{tmp}/review.json", "HOME": f"{tmp}/home",
                                                      "PATH": os.environ["PATH"]})


# Conary's cn, for the merge train: it records how it was run, then answers
# what CN says for its subcommand (show, join, leave, step), or fails with
# that subcommand's exit code. With "token", it first runs its token command
# for the repo, as cn does, and records what that did.
CN, CN_LOG = f"{tmp}/cn.json", f"{tmp}/cn-log"
with open(f"{tmp}/cn", "w") as f:
    f.write(f"""#!{sys.executable}
import json, os, subprocess, sys
args = sys.argv[1:]
do = json.load(open({CN!r}))
token = None
if do.get("token"):
    p = subprocess.run(os.environ["CONARY_TOKEN_COMMAND"].split() + [args[2]], capture_output=True, text=True)
    token = {{"rc": p.returncode, "out": p.stdout, "err": p.stderr}}
with open({CN_LOG!r}, "a") as f:
    f.write(json.dumps({{"args": args, "env": {{k: v for k, v in os.environ.items() if k.startswith("CONARY_")}},
                        "token": token}}) + "\\n")
if do.get("rc", {{}}).get(args[1]):
    print("cn: " + args[1] + " failed", file=sys.stderr)
    sys.exit(do["rc"][args[1]])
print(json.dumps(do[args[1]]))
""")
os.chmod(f"{tmp}/cn", 0o755)
IDLE = {"conflicts": [], "verdicts": [], "red": [], "landed": [], "refused": [], "unready": [], "finished": [],
        "now": {"state": "idle"}}


def cn(**do):
    """What cn answers from now on (an empty train, a PR joined, left, an
    idle step, unless told otherwise), and a log that starts over."""
    with open(CN, "w") as f:
        json.dump({"show": {"joined": [], "in_flight": None, "candidates": []}, "join": {"joined": True},
                   "leave": {"left": True}, "step": IDLE, **do}, f)
    open(CN_LOG, "w").close()


def cn_runs():
    """How cn was run since its answers were last set, its arguments alone."""
    return [json.loads(x)["args"] for x in open(CN_LOG)]


def cn_log():
    return [json.loads(x) for x in open(CN_LOG)]


def agent(**do):
    """What the stand-in codex does from now on, and a log that starts over."""
    with open(f"{tmp}/home/.fake/codex.json", "w") as f:
        json.dump(do, f)
    open(f"{tmp}/home/.fake/codex.log", "w").close()


def agent_runs():
    return [json.loads(x) for x in open(f"{tmp}/home/.fake/codex.log")]


agent()
cn()
# The module's default one-way paths.
with open(sys.argv[2]) as f:
    ONE_WAY = json.load(f)
with open(f"{tmp}/config.json", "w") as f:
    json.dump({
        "owners": ["sam"], "exclude": [], "context": "review-settled", "rulesets": True, "keepBranches": [],
        "app": {"id": 7, "slug": "steward"},
        "baselineJobs": False, "baselinePerPass": 1, "authors": ["sam"], "bots": ["dependabot", "renovate", "workers"], "machinery": None,
        "stateDir": f"{tmp}/state", "snapDir": f"{tmp}/trees", "majorKey": f"{tmp}/sweep.key", "projectsDir": f"{tmp}/none", "jobsDir": f"{tmp}/jobs", "renovateEmail": "bot@renovateapp.com", "proj": "proj", "jobTimeout": 60, "jobIdle": 20, "proofTimeout": 180, "heavy": None, "owner": "Sam", "cleanBranches": False,
        "labels": {"hold": "hold", "owner": "needs-owner", "first": "first", "waiting": {"releaseAge": "waiting: release age", "proofSlot": "waiting: proof slot", "reviewer": "waiting: reviewer"}}, "notify": {"repo": "", "assignee": "", "label": "autoland"}, "inbox": {"enable": False, "label": "claude", "unstickLabel": "unstick", "authors": ["sam"]},
        "dashboard": {"enable": False, "waitDays": 4, "errorDays": 1, "title": "Dependency Dashboard",
                      "renovateKick": "http://127.0.0.1:9/kick/renovate", "renovate": None,
                      "renovateRuns": f"{tmp}/renovate-runs"},
        "flaky": {"enable": False, "bin": f"{tmp}/flaky", "jobTimeout": 240},
        "defaults": {"rounds": 5,
                     "oneWay": ONE_WAY, "handoffAfter": 30, "sessionHandoffAfter": 90, "settleDelay": 3,
                     "stallAfter": 120, "holdStallAfter": 480, "renovateMergeAfter": 60,
                     "directPush": False, "ruleset": True, "requireCi": True,
                     "proofJobs": {}, "proofBuilds": {}, "codex": True, "rival": True, "skipPaths": [],
                     "train": False, "trainSize": 8, "trainRequire": [], "trainStallAfter": 240},
        "trainContext": "conary/train",
        "train": {"cn": f"{tmp}/cn", "env": {"CONARY_LEDGER": f"{tmp}/ledger", "CONARY_ANCHOR_REPO": "sam/homelab"},
                  "branches": "conary/*"},
        "repos": {}, "pipelineRepos": ["Sam/Pipe"], "leadRepos": ["Sam/Lead"],
        "webhook": {"url": "https://example.org/burrow/hook", "events": ["check_suite", "push"]},
        "kick": f"http://127.0.0.1:{kicks.server_port}/kick/autoland",
        "watch": {"kick": f"http://127.0.0.1:{kicks.server_port}/kick/watchdog", "escalated": f"{tmp}/escalated.json",
                  "renovateSweep": True},
        "renovateRun": 7200,
        "reviewers": {"codex": {"enable": True}, "rival": {"enable": True}},
        "skipPaths": ["(^|/)secrets?/", r"\.(pem|key|p12|pfx)$", r"(^|/)\.env(\..*)?$"],
        "nix": f"{tmp}/nix",
    }, f)
os.environ["AUTOLAND_CONFIG"] = f"{tmp}/config.json"
os.environ["CREDENTIALS_DIRECTORY"] = f"{tmp}/cred"
os.environ["HOME"] = f"{tmp}/home"
os.environ["GH_TOKEN"] = "ghp_steward_token"  # never passed on to an agent

sys.path.insert(0, os.path.dirname(os.path.abspath(sys.argv[1])))
sys.argv = ["steward"]
import branches as B  # noqa: E402,F401
import common as C  # noqa: E402,F401
import prs as P  # noqa: E402,F401
import repos as R  # noqa: E402,F401
import reviewers as V  # noqa: E402,F401
import step as H  # noqa: E402,F401
import steward as S  # noqa: E402,F401
import touches as G  # noqa: E402,F401
REAL_GH = C.gh  # the real one, before any check stands in for it
REAL_TOKEN = C.token
# The App's installation tokens, one an owner (passes.py checks the real one).
C.token = lambda owner: f"ghs_app_{owner}"
# A second reviewer beside Codex, standing in for any a machine adds: the
# steward settles a PR on every reviewer it has, so its checks need two to
# tell one's word from the other's. review.py runs the one real agent.
V.NAMES["rival"] = "Rival"

for d in (V.QUEUE, V.SNAP, V.TREES, V.DONE, V.SESSIONS, V.GIT):
    os.makedirs(os.path.join(f"{tmp}/state", d))


def sh(*a, cwd=None):
    return subprocess.run(a, cwd=cwd, check=True, capture_output=True, text=True).stdout


head = "a" * 40
WROTE, JOBS, POSTED, FORGOT = [], [], [], []
SINCE = []  # each job's place in line (C.job's since), in the order they started
DESCRIBED = []  # the description each posted review was shown (its fingerprint)
COMMITS = {}  # a PR's commits' author emails, by number, as `pr view --json commits` gives them (None: the ask fails)
PR_DIFF = {"v": ""}  # what `gh pr diff` gives


ACTING = []  # the owner each stood-in call to GitHub was made for


def acting():
    """Every call to GitHub is made for an owner (C.acting_for): one that
    isn't would go as the machine user's own login."""
    assert C._for, "a call to GitHub outside acting_for"
    ACTING.append(C._for[-1])


# Where a PR's head leaves its base, as GitHub's compare says, by the base
# branch's name ("v" for any other): None, it says nothing. FORKS: its
# answers in turn, before FORK's, while there are any.
FORK, FORKS = {"v": "f" * 40}, []
RULES = {}  # the rules on a branch of sam/r, by its name in the path: none unless set, or what GitHub raises
PROTECTION = {}  # the same for a branch's classic protection: a 404 unless set


def gh_stub(*a, **k):
    acting()
    if a[:1] == ("api",) and a[1].startswith("repos/sam/r/rules/branches/"):
        got = RULES.get(a[1].removeprefix("repos/sam/r/rules/branches/").partition("?")[0], [])
        if isinstance(got, Exception):
            if k.get("check", True):
                raise got
            return None  # gh's failure, logged
        return [got]  # --slurp: the pages
    if a[:1] == ("api",) and a[1].startswith("repos/sam/r/branches/") and a[1].endswith("/protection"):
        got = PROTECTION.get(a[1].removeprefix("repos/sam/r/branches/").removesuffix("/protection"),
                             RuntimeError(f"gh api {a[1]}: gh: Branch not protected (HTTP 404)"))
        if isinstance(got, Exception):
            if k.get("check", True):
                raise got
            return None
        return got
    if a[:2] == ("pr", "view") and "commits" in a:
        got = COMMITS.get(int(a[2]), ["bot@renovateapp.com"])
        return None if got is None else {"commits": [{"authors": [{"email": e}]} for e in got]}
    if a[:2] == ("api", "repos/sam/r"):
        return {"allow_auto_merge": True, "delete_branch_on_merge": True, "allow_squash_merge": True}
    if a[:2] == ("pr", "diff"):
        return PR_DIFF["v"]
    if a[:1] == ("api",) and a[1].startswith("repos/sam/r/compare/") and "{fork: .merge_base_commit.sha}" in a:
        # Read as GitHub reads its URL: a `#` ends it, a `?` starts its
        # query, and what's left is decoded once.
        path = a[1].removeprefix("repos/sam/r/compare/").partition("#")[0].partition("?")[0]
        base, dots, _ = path.partition("...")
        fork = FORKS.pop(0) if FORKS else FORK.get(urllib.parse.unquote(base), FORK["v"]) if dots else None
        return {"fork": fork} if fork else None
    return ""


FAIL = []  # writes that fail, by a piece of their arguments: gh's None, or with check what gh() raises
# What GitHub says to each, as gh prints it: two lines, as it can be.
REFUSAL = "Repository rule violations found\nMerge method squash is not allowed for this base branch (HTTP 405)"


def write(*a, **k):
    acting()
    WROTE.append(" ".join(a))
    if not any(f in " ".join(a) for f in FAIL):
        return {}
    if k.get("check", True):
        raise RuntimeError(f"gh {' '.join(a[:3])}: gh: {REFUSAL}")
    return None


def job(repo, task, clone=False, since=None):
    JOBS.append(task)
    SINCE.append(since)
    return f"job-{len(JOBS)}"


# The snapshot and the posting have their own checks (review.py, post.py):
# for handle() a queued review is a file, and a posted one is a line in POSTED.
STUB = {"queue": None, "post": None}  # an error each raises, when set


def queue(repo, pr, key, r):
    if STUB["queue"]:
        raise STUB["queue"]
    C.save(f"{V.QUEUE}/{V.item_name(key, r, pr['headRefOid'], pr['baseRefName'])}.json",
           {"reviewer": r, "repo": repo, "number": pr["number"], "headRefOid": pr["headRefOid"], "baseRefName": pr["baseRefName"],
            "description": C.fingerprint(pr["body"])})


def post(repo, pr, r, answer, description):
    if STUB["post"]:
        raise STUB["post"]
    if not isinstance(answer, dict) or not isinstance(answer.get("findings"), list):
        raise ValueError("not a review")
    POSTED.append((r, pr["headRefOid"], len(answer["findings"])))
    DESCRIBED.append(description)
    return "findings" if answer["findings"] else "clean"


STATES = {}  # a job's state, by id, as `proj state` says it: running unless set
STOPPED = []  # the jobs `proj stop` ended, in order
UNSTOPPABLE = set()  # the jobs `proj stop` can't stop


def stop_job(jid):
    """proj stop: the job's over, and `proj state` says it failed (its turn's
    unit was stopped before it wrote an outcome); one in UNSTOPPABLE is
    refused."""
    if jid in UNSTOPPABLE:
        return False
    STOPPED.append(jid)
    STATES[jid] = "failed"
    return True


T0 = "2026-10-24T00:00:00Z"
# The times a PR's status names (H.clock) are in the machine's zone: UTC here.
os.environ["TZ"] = "UTC"
time.tzset()
T = C.ts(T0)
info = {"squashMergeAllowed": True, "mergeCommitAllowed": False, "rebaseMergeAllowed": False,
        "defaultBranchRef": {"name": "main"}}


# The steward's App, as GraphQL gives its posts' author (no "[bot]").
STEWARD = {"login": "steward", "__typename": "Bot"}


def rv_review(r, k, sha=head, at="2026-10-24T00:11:00Z", who=None, base="main", body=""):
    """A review the steward posted for reviewer r, with k findings, shown
    the description `body`; with who, one a user of that login posted with
    the same marker."""
    author = STEWARD if who is None else {"login": who, "__typename": "User"}
    return {"author": author, "state": "COMMENTED", "submittedAt": at, "commit": {"oid": sha},
            "body": f"<!-- {r}-review:v1 " + json.dumps({"commit_sha": sha, "base": base, "findings": k,
                                                          "description": C.fingerprint(body)}) + " -->"}


def judgment(v, n=6, sha=head, base="main", body="", on=None):
    """A judge's verdict line, sealed as its task gives it: on the PR's base
    and the description `body`, or on `on` (the base alone, as they were
    before descriptions were judged)."""
    on = on or f"{base} {C.fingerprint(body)}"
    return f"<!-- autoland-judgment: {v} {sha} {C.seal('sam/r', n, 'autoland-judgment', v, sha, on)} -->"


def swept(v, n=8, sha=head, base="main"):
    """renovate-sweep's verdict line, sealed as major-seal seals it."""
    return f"<!-- major-verdict: {v} {sha} on {base} {C.seal('sam/r', n, 'major-verdict', v, sha, base)} -->"


def said(body, who="sam"):
    """A PR comment, as the query gives it."""
    return {"author": {"login": who}, "createdAt": T0, "updatedAt": T0, "databaseId": 2, "body": body}


CI_GREEN = [{"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"}]


def mkpr(reviews=(), comments=(), threads=(), draft=False, number=5, sha=head, files=("a.py",), labels=(),
         branch="feat", title="t", checks=CI_GREEN, base="main", body="", suites=(), renames=()):
    return {
        "id": "x", "number": number, "title": title, "url": "u", "isDraft": draft, "isCrossRepository": False,
        "headRefName": branch, "headRefOid": sha, "baseRefName": base, "createdAt": T0, "updatedAt": T0, "body": body,
        "mergeStateStatus": "CLEAN", "author": {"login": "sam"}, "autoMergeRequest": None, "mergeQueueEntry": None,
        "labels": {"pageInfo": {"hasNextPage": False}, "nodes": [{"name": x} for x in labels]},
        "files": {"nodes": [{"path": f, "changeType": "MODIFIED"} for f in files]},
        "changedFiles": len(files), "reactions": {"nodes": []}, "timelineItems": {"nodes": list(renames)}, "comments": {"pageInfo": {"hasPreviousPage": False}, "nodes": list(comments)},
        "reviewThreads": {"pageInfo": {"hasNextPage": False}, "nodes": [{"isResolved": r} for r in threads]},
        "reviews": {"pageInfo": {"hasPreviousPage": False}, "nodes": list(reviews)},
        "commits": {"nodes": [{"commit": {"oid": sha, "statusCheckRollup": {"contexts": {
            "pageInfo": {"hasNextPage": False}, "nodes": list(checks)}},
            "checkSuites": {"pageInfo": {"hasNextPage": False}, "nodes": list(suites)}}}]},
    }


def clean(sha=head, comments=(), reviews=(), **kw):
    """A PR both reviewers found nothing on, at `sha` on its base, shown its description."""
    base, body = kw.get("base", "main"), kw.get("body", "")
    return mkpr(reviews=[*reviews, rv_review("codex", 0, sha, base=base, body=body), rv_review("rival", 0, sha, base=base, body=body)],
                comments=comments, sha=sha, **kw)


QDIR, DDIR = f"{tmp}/state/{V.QUEUE}", f"{tmp}/state/{V.DONE}"


def run(pr, after, obsolete=False):
    """One pass, `after` seconds after the head first showed up."""
    C.NOW = C.ts(T0) + after
    WROTE.clear()
    JOBS.clear()
    SINCE.clear()
    C.WAKE.clear()
    C.LIMITED.clear()
    POSTED.clear()
    digest = []
    with C.acting_for("sam/r"):
        H.handle("sam/r", pr, info, digest, obsolete)
    return " ".join(x for x in WROTE if "/statuses/" in x), digest


def merges():
    """The merges written: the merge endpoint's, the one way the steward merges."""
    return [x for x in WROTE if x.startswith("api -X PUT ") and x.split()[3].endswith("/merge")]


def merged():
    return bool(merges())


def queued():
    """The reviewers queued, one entry a review."""
    return sorted(x.partition("--")[0] for x in os.listdir(QDIR))


def worker(**back):
    """autoland-review, through the queue: each reviewer answers what
    `back` says it does, a clean review unless told otherwise."""
    for x in os.listdir(QDIR):
        r = x.partition("--")[0]
        item = json.load(open(f"{QDIR}/{x}"))
        said = back.get(r, {"result": "review", "answer": {"findings": []}})
        # A review carries the description it was shown, as the service's do.
        shown = {"description": item["description"]} if said["result"] == "review" else {}
        C.save(f"{V.DONE}/{x}", {**shown, **said, "at": C.NOW, "head": item.get("headRefOid"), "base": item.get("baseRefName")})
        os.remove(f"{QDIR}/{x}")


def pr_state(n=5):
    return json.load(open(f"{tmp}/state/pr-sam_r-{n}.json"))


def fresh_pr_state():
    shutil.rmtree(f"{tmp}/state")
    shutil.rmtree(V.TREES)
    for d in (V.QUEUE, V.SNAP, V.TREES, V.DONE, V.SESSIONS, V.GIT):
        os.makedirs(os.path.join(f"{tmp}/state", d))


REAL = {"job": C.job, "stop_job": C.stop_job, "forget_issues": C.forget_issues, "app_hook": R.app_hook}  # the steward's own, for the checks of what it runs


def stand_in():
    """gh, Claude jobs, the queue, the posting and the App's webhook, for
    handle() and the passes."""
    C.gh = gh_stub
    C.write = write
    C.job = job
    R.app_hook = lambda: None
    C.job_state = lambda jid: STATES.get(jid, "running")
    C.stop_job = stop_job
    C.forget_issues = FORGOT.append
    V.queue = queue
    V.post = post
