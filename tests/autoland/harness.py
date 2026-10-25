"""What the steward's checks share (check.py runs them): its config and
state in a temp dir, stand-ins for gh, Claude jobs, DeepSeek's API and
burrow-events' kick port, and PRs shaped like its query's."""

import http.server
import json
import os
import shutil
import sys
import tempfile
import threading

tmp = tempfile.mkdtemp()

# DeepSeek: answers every request with ANSWER, as JSON in the message.
REQS, ANSWER = [], {}


class API(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        req = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        REQS.append((self.headers["Authorization"], req, self.path))
        if "tools" in req and ANSWER.get("noBeta"):
            self.send_response(400)
            self.end_headers()
            return
        # ANSWER["junk"]: that many answers that aren't JSON come first;
        # ANSWER["long"]: efforts that think past max_tokens.
        junk = ANSWER.get("junk", 0) > 0
        ANSWER["junk"] = ANSWER.get("junk", 0) - 1
        long = req["reasoning_effort"] in ANSWER.get("long", ())
        text = "" if junk or long else json.dumps(ANSWER["v"])
        if "tools" in req and not long:
            msg = {"content": "", "tool_calls": [{"type": "function", "function": {"name": "submit_review", "arguments": text}}]}
        else:
            msg = {"content": text}
        why = "length" if long else "tool_calls" if "tools" in req else "stop"
        # A long call keeps the line alive with blank lines first.
        out = b"\n\n" + json.dumps({"choices": [{"message": msg, "finish_reason": why}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(out)

    def log_message(self, *a):
        pass


srv = http.server.HTTPServer(("127.0.0.1", 0), API)
threading.Thread(target=srv.serve_forever, daemon=True).start()

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
with open(f"{tmp}/cred/deepseek", "w") as f:
    f.write('DEEPSEEK_API_KEY=sk-test\nDEEPSEEK_MODEL="m-test"\n')
with open(f"{tmp}/cred/webhook", "w") as f:
    f.write("hook-secret\n")
# The module's default one-way paths.
with open(sys.argv[2]) as f:
    ONE_WAY = json.load(f)
with open(f"{tmp}/config.json", "w") as f:
    json.dump({
        "owners": ["sam"], "exclude": [], "context": "review-settled", "rulesets": True, "keepBranches": [],
        "baselineJobs": False, "baselinePerPass": 1, "authors": ["sam", "dependabot", "renovate"], "codexLogin": "chatgpt-codex-connector",
        "stateDir": f"{tmp}/state", "projectsDir": f"{tmp}/none", "renovateEmail": "bot@renovateapp.com", "proj": "proj", "owner": "Sam", "cleanBranches": False,
        "labels": {"hold": "hold", "owner": "needs-owner"}, "notify": {"repo": "", "assignee": "", "label": "autoland"},
        "defaults": {"rounds": 5,
                     "oneWay": ONE_WAY, "handoffAfter": 30, "settleDelay": 3, "codexTimeout": 45,
                     "directPush": False, "ruleset": True, "requireCi": True,
                     "proofJobs": {}, "deepseek": True},
        "repos": {},
        "webhook": {"url": "https://example.org/burrow/hook", "events": ["check_suite", "push"]},
        "kick": f"http://127.0.0.1:{kicks.server_port}/kick/autoland",
        "deepseek": {"enable": True, "baseUrl": f"http://127.0.0.1:{srv.server_port}", "effort": "max",
                     "skipPaths": ["(^|/)secrets?/", r"\.(pem|key|p12|pfx)$", r"(^|/)\.env(\..*)?$"], "maxDiffChars": 800000, "parallel": 2, "maxTokens": 1000,
                     "timeout": 30, "caFile": None},
    }, f)
os.environ["AUTOLAND_CONFIG"] = f"{tmp}/config.json"
os.environ["CREDENTIALS_DIRECTORY"] = f"{tmp}/cred"

sys.path.insert(0, os.path.dirname(os.path.abspath(sys.argv[1])))
sys.argv = ["steward"]
import common as C  # noqa: E402
import deepseek as D  # noqa: E402
import repos as R  # noqa: E402,F401 (for the checks)
import steward as S  # noqa: E402
import touches as G  # noqa: E402,F401 (for the checks)
D.ssl.create_default_context = lambda cafile=None: None  # plain http here
head = "a" * 40

WROTE, RUNS, JOBS, RESULT = [], [], [], {"v": "clean"}
CUR = {"sha": head}  # the PR's head as the DeepSeek worker finds it


COMMITS = {}  # a PR's commits' author emails, by number, as `pr view --json commits` gives them


PR_DIFF = {"v": ""}  # what `gh pr diff` gives


def gh_stub(*a, **k):
    if a[:2] == ("api", "user"):
        return "sam"
    if a[:2] == ("pr", "view") and "commits" in a:
        return {"commits": [{"authors": [{"email": e}]} for e in COMMITS.get(int(a[2]), ["bot@renovateapp.com"])]}
    if a[:2] == ("api", "repos/sam/r"):
        return {"allow_auto_merge": True, "delete_branch_on_merge": True, "allow_squash_merge": True}
    if a[:2] == ("pr", "diff"):
        return PR_DIFF["v"]
    if a[:2] == ("api", "repos/sam/r/pulls/5"):
        return {"state": "open", "sha": CUR["sha"]}
    return ""


FAIL = []  # writes that fail (gh's None), by a piece of their arguments


def write(*a, **k):
    WROTE.append(" ".join(a))
    return None if any(f in " ".join(a) for f in FAIL) else {}


def run_deepseek(repo, pr):
    RUNS.append(pr["headRefOid"])
    if RESULT["v"] == "down":
        raise RuntimeError("down")
    return RESULT["v"]


def job(repo, task, clone=False):
    JOBS.append(task)
    return f"job-{len(JOBS)}"


STATES = {}  # a job's state, by id, as `proj state` says it: running unless set
T0 = "2026-10-24T00:00:00Z"
T = C.ts(T0)
info = {"squashMergeAllowed": True, "mergeCommitAllowed": False, "rebaseMergeAllowed": False}


def codex_clean(sha=head):
    return {"author": {"login": "chatgpt-codex-connector"}, "createdAt": T0, "updatedAt": T0, "databaseId": 1,
            "body": "<!-- codex-review-state:v1 " + json.dumps(
                {"code_review": {"commit_sha": sha, "has_any_findings": False, "status": "completed"}}) + " -->"}


def ds_review(k, sha=head, at="2026-10-24T00:11:00Z"):
    return {"author": {"login": "sam"}, "state": "COMMENTED", "submittedAt": at, "commit": {"oid": sha},
            "body": "<!-- deepseek-review:v1 " + json.dumps({"commit_sha": sha, "findings": k}) + " -->"}


def judgment(v, n=6, sha=head, base="main"):
    """A judge's verdict line, sealed as its task gives it."""
    return f"<!-- autoland-judgment: {v} {sha} {C.seal('sam/r', n, 'autoland-judgment', v, sha, base)} -->"


def said(body, who="sam"):
    """A PR comment, as the query gives it."""
    return {"author": {"login": who}, "createdAt": T0, "updatedAt": T0, "databaseId": 2, "body": body}


CI_GREEN = [{"__typename": "CheckRun", "name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS"}]


def mkpr(reviews=(), comments=(), threads=(), draft=False, number=5, sha=head, files=("a.py",), labels=(),
         branch="feat", title="t", checks=CI_GREEN, base="main"):
    return {
        "id": "x", "number": number, "title": title, "url": "u", "isDraft": draft, "isCrossRepository": False,
        "headRefName": branch, "headRefOid": sha, "baseRefName": base, "createdAt": T0, "updatedAt": T0,
        "mergeStateStatus": "CLEAN", "author": {"login": "sam"}, "autoMergeRequest": None,
        "labels": {"pageInfo": {"hasNextPage": False}, "nodes": [{"name": x} for x in labels]},
        "files": {"nodes": [{"path": f, "changeType": "MODIFIED"} for f in files]},
        "changedFiles": len(files), "reactions": {"nodes": []}, "comments": {"pageInfo": {"hasPreviousPage": False}, "nodes": list(comments)},
        "reviewThreads": {"pageInfo": {"hasNextPage": False}, "nodes": [{"isResolved": r} for r in threads]},
        "reviews": {"pageInfo": {"hasPreviousPage": False}, "nodes": list(reviews)},
        "commits": {"nodes": [{"commit": {"oid": sha, "statusCheckRollup": {"contexts": {
            "pageInfo": {"hasNextPage": False}, "nodes": list(checks)}}}}]},
    }


def clean(sha=head, comments=(), **kw):
    """A PR both reviewers found nothing on, at `sha`."""
    return mkpr(reviews=[ds_review(0, sha)], comments=[codex_clean(sha), *comments], sha=sha, **kw)


QDIR, DDIR = f"{tmp}/state/{D.QUEUE}", f"{tmp}/state/{D.DONE}"


def run(pr, after):
    """One pass, `after` seconds after the head first showed up."""
    C.NOW = C.ts(T0) + after
    WROTE.clear()
    RUNS.clear()
    JOBS.clear()
    C.WAKE.clear()
    digest = []
    S.handle("sam/r", pr, info, digest)
    return " ".join(x for x in WROTE if "/statuses/" in x), digest


def merged():
    return any(x.startswith("pr merge") for x in WROTE)


def queued():
    return [json.load(open(f"{QDIR}/{x}"))["headRefOid"] for x in os.listdir(QDIR) if x.endswith(".json")]


def worker():
    """autoland-deepseek, through the queue."""
    RUNS.clear()
    D.deepseek_queue()
    assert queued() == []


def fresh_pr_state():
    shutil.rmtree(f"{tmp}/state")
    for d in (QDIR, DDIR):
        os.makedirs(d)


REAL = {"job": C.job}  # the steward's own, for the checks of what it runs


def stand_in():
    """gh, Claude jobs and DeepSeek's reviews, for handle() and the passes."""
    C.gh = gh_stub
    C.write = write
    C.job = job
    C.job_state = lambda jid: STATES.get(jid, "running")
    D.run_deepseek = run_deepseek
