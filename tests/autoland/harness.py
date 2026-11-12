"""What the steward's checks share (check.py runs them): its config and
state in a temp dir; stand-ins for gh, Claude jobs, GitHub's git remote and
Conary's `cn` (each records how it was run, then does what it's told) and
burrow-events' kick port; the reviewers' service, with its own stand-ins
for the agent and its sandbox; and PRs shaped like its query's."""

import http.server
import json
import os
import pathlib
import shlex
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse

tmp = tempfile.mkdtemp()
SETTINGS = os.path.abspath(sys.argv[9])


def admission(allowed, pipeline=(), until=None):
    """Use the production Rust reader, with a tracked file and live overlay."""
    folder = tempfile.mkdtemp(dir=tmp)
    path = f"{folder}/tracked.json"
    overlay = f"{folder}/overlay.json"
    with open(path, "w") as f:
        json.dump({"overlay": overlay, "allowed": allowed, "pipelineRepos": list(pipeline)}, f)
    if until is not None:
        subprocess.run([SETTINGS, path, "pipeline", str(until)], check=True, capture_output=True)
    return {"command": SETTINGS, "config": path}


# Existing scenarios opt their fixture repos in; production has no such list.
DEFAULT_ADMISSION = admission([
    "sam/r", "sam/other", "sam/q", "sam/u", "sam/up", "sam/uu", "sam/p",
    "sam/app", "sam/later", "sam/new", "sam/fresh", "sam/tool", "sam/swept",
    "sam/old", "sam/was", "sam/took", "sam/config", "sam/.cfg", "sam/.github",
    "sam/empty", "sam/alias", "sam/plain", "sam/notes", "sam/now", "sam/latest",
    "sam/mirrored", "sam/lo-k", "sam/flaky", "sam/homelab", "sam/absent", "sam/bad",
    "sam/gone", "sam/nobody", "sam/none", "sam/skip", "sam/pipe", "sam/lead",
    "eve/app", "eve/later", "eve/p", "eve/tool", "eve/permission", "eve/.cfg",
    "kim/busy", "kim/r", "kim/x", "kim/z", "old-org/app", "old-org/later", "old-org/swept",
])

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
# burrow-upkeep (argv[5]), which a pass runs on each repo, and its source
# (argv[6], crates/burrow-upkeep/src).
UPKEEP, UPKEEP_SRC = sys.argv[5], sys.argv[6]
# burrow-proof (argv[7]), which starts a proof run's unit (runs.py).
PROOF_RUN = os.path.abspath(sys.argv[7])
# The Rust question rows consumed by the owner digest.
PROJ_ASK_SRC = sys.argv[8]
# What the passes here run in its place (upkeeping.py runs the real one): it
# notes how it was run (UPKEEP_RAN), with what `branches` and `hooks` read on
# its stdin last, and finds nothing due, stuck or wanted; its `claims` keeps
# every fix job as it was and says nothing of any (unpushed.py runs the real
# one). Its `token` is the real one's, and so are its `sweep` and `verdict`,
# which ask GitHub nothing, its `carry`, which reads the stand-in for
# GitHub's git remote, and its `commits`, which reads GitHub's compare as
# serve_compare has it, on the config upkeep_config writes (UPKEEP_REAL).
# Those it starts as they are, from sh: every pass's PRs read their verdicts,
# and a Python's start for each would be most of the Python check's time.
UPKEEP_RAN, UPKEEP_REAL, UPKEEP_FAKE = f"{tmp}/upkeep-ran", f"{tmp}/upkeep-real.json", f"{tmp}/upkeep-fake"
# GitHub's compare, as gh answers the real one's `commits` with it: page N
# of what serve_compare last said (COMPARE_PAGES), with a Link to the next,
# each page asked for noted (COMPARE_CALLED); no answer for None.
COMPARE_BIN, COMPARE_PAGES, COMPARE_CALLED = f"{tmp}/compare-bin", f"{tmp}/compare.json", f"{tmp}/compare-called"
os.makedirs(COMPARE_BIN)
with open(f"{COMPARE_BIN}/gh", "w") as f:
    f.write(f"""#!{sys.executable}
import json, sys, urllib.parse
path = sys.argv[5]
with open({COMPARE_CALLED!r}, "a") as f:
    f.write(path + "\\n")
pages = json.load(open({COMPARE_PAGES!r}))
query = urllib.parse.parse_qs(path.partition("?")[2])
n = int(query.get("page", ["1"])[0])
if not path.startswith("repos/sam/r/compare/") or pages is None or n > len(pages):
    sys.stderr.write("no answer scripted\\n")  # gh's failure, which isn't asked again
    sys.exit(1)
link = f'Link: <https://api.github.com/{{path.partition("?")[0]}}?per_page=100&page={{n + 1}}>; rel="next"\\r\\n' if n < len(pages) else ""
sys.stdout.write("HTTP/1.1 200 OK\\r\\n" + link + "\\r\\n" + json.dumps(pages[n - 1]))
""")
os.chmod(f"{COMPARE_BIN}/gh", 0o755)
# burrow-upkeep as every pass here runs the real one: its `commits` asks
# that gh, whatever gh a test puts first for the rest.
UPKEEP_RUN = f"{tmp}/upkeep-run"
with open(UPKEEP_RUN, "w") as f:
    f.write(f"""#!/bin/sh
[ "$2" = commits ] && PATH={shlex.quote(COMPARE_BIN)}:$PATH
[ "$2" = ci ] && PATH={shlex.quote(tmp + "/ci-bin")}:$PATH
exec {shlex.quote(UPKEEP)} "$@"
""")
os.chmod(UPKEEP_RUN, 0o755)
with open(UPKEEP_FAKE, "w") as f:
    f.write(f"""import json, sys
if sys.argv[2] == "claims":
    look = json.load(sys.stdin)
    print(json.dumps({{"answering": look["answering"], "pushedBy": look["pushedBy"], "stop": [], "answered": [],
                      "wake": None, "limited": None}}))
    sys.exit(0)
with open({UPKEEP_RAN!r}, "a") as f:
    f.write(json.dumps(sys.argv[2:] + ([json.load(sys.stdin)] if sys.argv[2] in ("branches", "hooks") else [])) + "\\n")
print(json.dumps({{"failed": [], "limited": None}} if sys.argv[2] in ("settle", "hooks") else
                 {{"stuck": [], "wake": None, "failed": [], "limited": None, "searched": False, "jobs": []}}))
""")
with open(f"{tmp}/upkeep-none", "w") as f:
    f.write(f"""#!/bin/sh
case "$2" in
token|sweep|verdict|carry|commits|ci) shift; exec {shlex.quote(UPKEEP_RUN)} {shlex.quote(UPKEEP_REAL)} "$@" ;;
esac
exec {shlex.quote(sys.executable)} -I {shlex.quote(UPKEEP_FAKE)} "$@"
""")
os.chmod(f"{tmp}/upkeep-none", 0o755)
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
        json.dump({"settings": json.load(open(C.CFG["settings"]["config"])), "queue": f"{tmp}/state/review/queue", "done": f"{tmp}/state/review/done",
                   "sessions": f"{tmp}/state/review/sessions", "snapshots": f"{tmp}/trees",
                   "reviewers": {"codex": {"enable": True, "effort": "high", "parallel": 2, "off": []}}, "timeoutMinutes": 5,
                   "agents": f"{tmp}/agents", "path": os.environ["PATH"], "sandbox": f"{tmp}/bwrap",
                   "box": {"store": f"{tmp}/store-paths", "caBundle": f"{tmp}/ca.crt", "links": {}},
                   "leadRepos": C.CFG["leadRepos"], "pipelineRepos": C.CFG["pipelineRepos"], "kick": C.CFG["kick"]}, f)
    subprocess.run([REVIEW, "work"], check=True, env={"REVIEW_CONFIG": f"{tmp}/review.json", "HOME": f"{tmp}/home",
                                                      "PATH": os.environ["PATH"]})


# Conary's cn: it records how it was run, then answers what CN says for its
# subcommand (the train's show, join, leave, step; class), or fails with that
# subcommand's exit code. With "token", it first runs its token command for
# the repo, as cn does, and records what that did. Unless CN says otherwise,
# `class` classes the files the PR at that head lists (TOUCHED, which mkpr
# and run keep) by the policy's R2, docs, notDocs and hidden paths, as Conary
# does (AGENTS.md, CLAUDE.md and .github/ are never docs; a hidden pattern
# also hides a directory's files, the directory with a trailing slash), and
# raises it by the PR's labels there, with the raises CN says the ledger
# kept from before ("kept"), as Conary does: `r2` makes it R2, `review` asks
# for a review round, and `raised` names them, sorted.
# GitHub's compare's pages for a PR, by number, as serve_compare takes them:
# unless set, its commits as COMMITS and COMMITTERS have them.
COMPARE = {}
CN, CN_LOG, TOUCHED = f"{tmp}/cn.json", f"{tmp}/cn-log", f"{tmp}/touched.json"
with open(f"{tmp}/cn", "w") as f:
    f.write(f"""#!{sys.executable}
import json, os, re, subprocess, sys
args = sys.argv[1:]
sub, repo = (args[1], args[2]) if args[0] == "train" else (args[0], args[1])
do = json.load(open({CN!r}))
token = None
if do.get("token"):
    p = subprocess.run(os.environ["CONARY_TOKEN_COMMAND"].split() + [repo], capture_output=True, text=True)
    token = {{"rc": p.returncode, "out": p.stdout, "err": p.stderr}}
with open({CN_LOG!r}, "a") as f:
    f.write(json.dumps({{"args": args, "env": {{k: v for k, v in os.environ.items() if k.startswith("CONARY_")}},
                        "token": token}}) + "\\n")
if do.get("rc", {{}}).get(sub):
    print("cn: " + sub + " failed", file=sys.stderr)
    sys.exit(do["rc"][sub])
if sub == "class" and "class" not in do:
    touched = json.load(open({TOUCHED!r})).get(args[2] + "@" + args[4], {{}})
    paths = touched.get("files", [])
    raised = sorted({{x.lower() for x in touched.get("labels", [])}} & {{"r2", "review"}} | set(do.get("kept", [])))
    policy = json.load(open(os.environ["CONARY_POLICY"]))
    rules = [policy, policy.get("repos", {{}}).get(repo, {{}})]
    pick = lambda key: [x for r in rules for x in r.get(key, [])]
    because = next((p for p in paths if any(re.search(x, p) for x in pick("r2"))), None)
    never = pick("notDocs") + [r"(^|/)AGENTS(\\.override)?\\.md$", r"(^|/)CLAUDE(\\.local)?\\.md$", r"^\\.github/"]
    held = lambda p: [p] + [p[:i + 1] for i, c in enumerate(p) if c == "/"]
    hidden = lambda p: any(re.search(x, h) for x in pick("hidden") for h in held(p))
    docs = bool(paths) and all(any(re.search(x, p) for x in pick("docs")) and not any(re.search(x, p) for x in never)
                               and not hidden(p) for p in paths)
    because = because or ("labelled r2" if "r2" in raised else None)
    do["class"] = {{"class": "R2" if because else "R0", "because": because, "docs": docs,
                   "review": bool(because) or not docs or "review" in raised, "raised": raised}}
print(json.dumps(do[sub]))
""")
os.chmod(f"{tmp}/cn", 0o755)
IDLE = {"conflicts": [], "verdicts": [], "red": [], "landed": [], "refused": [], "unready": [], "reseated": [],
        "finished": [], "now": {"state": "idle"}}


def cn(**do):
    """What cn answers from now on (an empty train, a PR joined, left, an
    idle step, unless told otherwise), and a log that starts over."""
    with open(CN, "w") as f:
        json.dump({"show": {"joined": [], "in_flight": None, "candidates": []}, "join": {"joined": True},
                   "leave": {"left": True}, "step": IDLE, **do}, f)
    open(CN_LOG, "w").close()


def cn_runs():
    """How cn was run for anything but a class since its answers were last
    set, its arguments alone."""
    return [x["args"] for x in cn_log()]


def cn_log():
    return [x for x in map(json.loads, open(CN_LOG)) if x["args"][0] != "class"]


def cn_classed():
    """The PRs cn was asked to class since its answers were last set, as
    (repo, number, head)."""
    return [(a[1], int(a[2]), a[4]) for a in (json.loads(x)["args"] for x in open(CN_LOG)) if a[0] == "class"]


def touching(number, sha, files, labels=()):
    """What `cn class` reads a PR at a head touches, and its labels."""
    touched = json.load(open(TOUCHED)) if os.path.exists(TOUCHED) else {}
    touched[f"{number}@{sha}"] = {"files": list(files), "labels": list(labels)}
    with open(TOUCHED, "w") as f:
        json.dump(touched, f)


def agent(**do):
    """What the stand-in codex does from now on, and a log that starts over."""
    with open(f"{tmp}/home/.fake/codex.json", "w") as f:
        json.dump(do, f)
    open(f"{tmp}/home/.fake/codex.log", "w").close()


def agent_runs():
    return [json.loads(x) for x in open(f"{tmp}/home/.fake/codex.log")]


agent()
cn()
# Each stage's label (labels.nix's labels.waiting), as the module gives them.
WAITING = {k: {"name": f"waiting: {x}", "description": x, "color": "c5def5"} for k, x in (
    ("review", "review"), ("reviewer", "reviewer"), ("fix", "fix"), ("ci", "CI"), ("proofSlot", "proof slot"),
    ("proof", "machine checks"), ("releaseAge", "release age"), ("train", "train"), ("merge", "merge"),
    ("owner", "Sam"), ("base", "base branch"))}
# The example machine's generated CONARY_POLICY, including hidden paths.
POLICY = sys.argv[2]
with open(f"{tmp}/config.json", "w") as f:
    json.dump({
        "settings": DEFAULT_ADMISSION,
        "owners": ["sam"], "exclude": [], "context": "review-settled", "rulesets": True,
        "app": {"id": 7, "slug": "steward"},
        "baselineJobs": False, "baselinePerPass": 1, "authors": ["sam"], "bots": ["dependabot", "renovate", "workers"], "machinery": None, "machineryLogin": None,
        "stateDir": f"{tmp}/state", "snapDir": f"{tmp}/trees", "majorKey": f"{tmp}/sweep.key", "projectsDir": f"{tmp}/none", "jobsDir": f"{tmp}/jobs", "renovateEmail": "bot@renovateapp.com", "proj": "proj", "jobTimeout": 60, "jobIdle": 20, "proofTimeout": 180, "heavy": None, "owner": "Sam",
        "labels": {"hold": "hold", "owner": "needs-owner", "first": "first", "batch": "upstream batch", "waiting": WAITING}, "notify": {"repo": "", "assignee": "", "label": "autoland"}, "inbox": {"enable": False, "labels": {"claude": "claude", "codex": "codex"}, "unstickLabel": "unstick", "postingAccountId": 2, "authors": [{"login":"sam","id":1}]},
        "renovate": None, "upkeep": [f"{tmp}/upkeep-none", "upkeep.json"],
        "defaults": {"rounds": 5,
                     "handoffAfter": 30, "sessionHandoffAfter": 90, "settleDelay": 3,
                     "stallAfter": 120, "holdStallAfter": 480, "renovateMergeAfter": 60,
                     "directPush": False, "ruleset": True, "requireCi": True,
                     "proofJobs": {}, "proofBuilds": {}, "proofRuns": {}, "codex": True, "rival": True,
                     "train": False, "trainSize": 8, "trainRequire": [], "trainNegative": None, "trainStallAfter": 240},
        "trainContext": "conary/train",
        "conary": {"cn": f"{tmp}/cn", "env": {"CONARY_LEDGER": f"{tmp}/ledger", "CONARY_ANCHOR_REPO": "sam/homelab",
                                              "CONARY_POLICY": POLICY}},
        "repos": {}, "pipelineRepos": ["Sam/Pipe"], "leadRepos": ["Sam/Lead"],
        "kick": f"http://127.0.0.1:{kicks.server_port}/kick/autoland",
        "watch": {"kick": f"http://127.0.0.1:{kicks.server_port}/kick/watchdog", "escalated": f"{tmp}/escalated.json",
                  "renovateSweep": True},
        "renovateRun": 7200,
        "reviewers": {"codex": {"enable": True}, "rival": {"enable": True}},
        "nix": f"{tmp}/nix",
        "proofRun": PROOF_RUN, "bash": shutil.which("bash"), "git": shutil.which("git"), "runsDir": f"{tmp}/runs",
    }, f)
os.environ["AUTOLAND_CONFIG"] = f"{tmp}/config.json"
os.environ["CREDENTIALS_DIRECTORY"] = f"{tmp}/cred"
os.environ["HOME"] = f"{tmp}/home"
os.environ["GH_TOKEN"] = "ghp_steward_token"  # never passed on to an agent

sys.path.insert(0, os.path.dirname(os.path.abspath(sys.argv[1])))
sys.argv = ["steward"]
import common as C  # noqa: E402,F401
import prs as P  # noqa: E402,F401
import repos as R  # noqa: E402,F401
import reviewers as V  # noqa: E402,F401
import step as H  # noqa: E402,F401
import steward as S  # noqa: E402,F401
import touches as G  # noqa: E402,F401
REAL_GH = C.gh  # the real one, before any check stands in for it
REAL_TOKEN = C.token
REAL_UPKEEP = C.upkeep
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


# The paths no reviewer reads, in burrow-upkeep's config (snapshot.rs).
SKIP = ["(^|/)secrets?/", r"\.(pem|key|p12|pfx)$", r"(^|/)\.env(\..*)?$"]


def upkeep_config(path=f"{tmp}/upkeep.json", **over):
    """burrow-upkeep's config, as the module writes it, on the steward's
    state, with `over` over it: the real binary's command, as the pass's
    config has it (C.CFG["upkeep"])."""
    with open(path, "w") as f:
        json.dump({"stateDir": C.STATE, "proj": f"{tmp}/upkeep-bin/proj", "projectsDir": C.CFG["projectsDir"],
                   "authors": C.CFG["authors"], "bots": C.CFG["bots"], "app": C.CFG["app"], "api": "http://127.0.0.1:9",
                   "webhook": {"url": "https://example.org/burrow/hook", "events": ["check_suite", "push"]},
                   "jobTimeout": C.CFG["jobTimeout"], "renovate": C.CFG["renovate"],
                   "dashboard": {"title": "Dependency Dashboard", "waitDays": 4, "errorDays": 1, "renovateKick": None, "renovateRuns": None},
                   "flaky": {"jobTimeout": 240},
                   "owner": C.CFG["owner"],
                   "settle": {"context": C.CFG["context"],
                              "trainContext": C.CFG["trainContext"], "rulesets": C.CFG["rulesets"], "labels": C.CFG["labels"],
                              "defaults": {k: C.CFG["defaults"][k] for k in ("directPush", "ruleset", "train")}, "repos": {}},
                   "branches": {"keep": ["gh-pages"], "repos": {}, "train": "conary/*"},
                   "git": f"{tmp}/origin", "snapshot": {"dir": V.TREES, "skipPaths": SKIP, "repos": {}}, **over}, f)
    return [UPKEEP_RUN, path]


upkeep_config(UPKEEP_REAL)

# A reviewer's session for a PR on its base, as burrow-review names it
# (queue.rs `session_name`), and the mark the sweep gives a closed PR's
# (queue.rs `GONE`).
GONE = ".gone-"


def session_name(r, snap):
    pr, _, base = snap.rsplit("-", 2)
    return f"{r}--{pr}-{base}"


head = "a" * 40
WROTE, JOBS, POSTED, FORGOT = [], [], [], []
SINCE = []  # each job's place in line (C.job's since), in the order they started
ROLES = []  # and the kind of work each is (C.job's role), which picks its model
DESCRIBED = []  # the description each posted review was shown (its fingerprint)
CARRY = {}  # what V.carry says of a head: nothing carries unless set
COMMITS = {}  # a PR's commits' author emails, by number, as GitHub's compare gives them (None: the ask fails)
COMMITTERS = {}  # and their committers' (None: the ask fails too)
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
ANNOTATIONS = {}  # a check run's annotations' messages, by its id: none unless set, or what GitHub raises


RERUNS = []  # the workflow runs (by id) the steward ran again
RERUN_FAILS = {}  # a run's id to what its rerun raises: none unless set
RUN_INFO = {}  # a run's id to GitHub's run_attempt, status, conclusion: attempt 1, cancelled, unless set


# The CI owner runs as compiled Rust, against an HTTP transport fixture.
CI_FIXTURE = f"{tmp}/ci.json"
os.makedirs(f"{tmp}/ci-bin")
with open(f"{tmp}/ci-bin/gh", "w") as f:
    f.write(f"#!{sys.executable}\n" + pathlib.Path(__file__).with_name("ci_github.py").read_text())
os.chmod(f"{tmp}/ci-bin/gh", 0o755)
os.environ["CI_FIXTURE"] = CI_FIXTURE


def ci_upkeep(*args, **kw):
    if args[0] != "ci":
        return REAL_UPKEEP(*args, **kw)

    def error(e):
        import re
        match = re.search(r"HTTP (\d+)", str(e))
        return {"error": str(e), "status": 503 if isinstance(e, C.Unavailable) else int(match[1]) if match else 422}

    data = {"runs": RUN_INFO, "failures": {k: error(v) for k, v in RERUN_FAILS.items()},
            "annotations": {k: error(v) if isinstance(v, Exception) else v for k, v in ANNOTATIONS.items()},
            "fork": FORK, "forks": FORKS, "reruns": [], "asked": [],
            "app": C.token(args[1].split("/")[0].lower()) if kw.get("as_app", True) and not C.DRY else None}
    with open(CI_FIXTURE, "w") as f:
        json.dump(data, f)
    old = C.CFG["upkeep"]
    C.CFG["upkeep"] = upkeep_config(f"{tmp}/ci-config.json")
    try:
        return REAL_UPKEEP(*args, **kw)
    finally:
        C.CFG["upkeep"] = old
        with open(CI_FIXTURE) as f:
            got = json.load(f)
        RUN_INFO.update(got["runs"])
        RERUNS.extend(got["reruns"])
        FORKS[:] = got["forks"]


C.upkeep = ci_upkeep


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
    if a[:2] == ("api", "repos/sam/r"):
        return {"allow_auto_merge": True, "delete_branch_on_merge": True, "allow_squash_merge": True}
    if a[:2] == ("pr", "diff"):
        return PR_DIFF["v"]
    return ""


def commit(sha, message, author="bot@renovateapp.com", committer="noreply@github.com"):
    """One commit as GitHub's REST API gives it."""
    return {"sha": sha, "commit": {"message": message, "author": {"email": author}, "committer": {"email": committer}}}


def compare_pages(commits, total=None, fork="f" * 40, shown=None):
    """A compare of a head with its base, its pages a hundred commits each:
    total_commits is the whole range's count, and GitHub may page through
    only the first `shown` of them."""
    listed = commits[:shown]
    return [{"total_commits": len(commits) if total is None else total, "merge_base_commit": {"sha": fork},
             "commits": listed[i:i + 100]} for i in range(0, max(len(listed), 1), 100)]


def fixture_pages(number, sha):
    """The PR's commits as COMMITS and COMMITTERS have them, the last at the
    head: one for each author and committer set, its first author and
    GitHub filling in. None if either fails."""
    authors = COMMITS.get(number, ["bot@renovateapp.com"])
    committers = COMMITTERS.get(number, ["noreply@github.com"])
    if authors is None or committers is None:
        return None
    n = max(len(authors), len(committers))
    return compare_pages([commit(sha if i == n - 1 else f"{i:040x}", "change: fixture",
                                 (authors + authors[:1] * n)[i], (committers + ["noreply@github.com"] * n)[i])
                          for i in range(n)])


def serve_compare(pages):
    """What GitHub's compare answers burrow-upkeep's `commits` with from now
    on, page by page (None: no answer), and its calls so far forgotten."""
    with open(COMPARE_PAGES, "w") as f:
        json.dump(pages, f)
    open(COMPARE_CALLED, "w").close()


def compare_calls():
    """The compare's pages burrow-upkeep asked for, since serve_compare."""
    return open(COMPARE_CALLED).read().splitlines()


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


def job(repo, task, clone=False, pipeline=True, timeout=None, since=None, *, role):
    JOBS.append(task)
    SINCE.append(since)
    ROLES.append(role)
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
    # Its review covers the PR's description if it was shown it (post.rs).
    return "findings" if answer["findings"] else "clean", description == C.fingerprint(pr["body"])


STATES = {}  # a job's state, by id, as `proj lifecycle` says it: running unless set
HELD = {}  # what a job holds open, by id, as `proj lifecycle` says it: nothing unless set
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
    HELD.pop(jid, None)  # proj stop clears what it held
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
         branch="feat", title="t", checks=CI_GREEN, base="main", body="", suites=(), unlabels=(), cut=False):
    touching(number, sha, files, labels)
    return {
        "id": "x", "number": number, "title": title, "url": "u", "isDraft": draft, "isCrossRepository": False,
        "headRefName": branch, "headRefOid": sha, "baseRefName": base, "createdAt": T0, "updatedAt": T0, "body": body,
        "mergeStateStatus": "CLEAN", "author": {"login": "sam"}, "autoMergeRequest": None, "mergeQueueEntry": None,
        "labels": {"pageInfo": {"hasNextPage": False}, "nodes": [{"name": x} for x in labels]},
        "files": {"nodes": [{"path": f, "changeType": "MODIFIED"} for f in files]},
        "changedFiles": len(files), "reactions": {"nodes": []}, "timelineItems": {"pageInfo": {"hasPreviousPage": cut}, "nodes": list(unlabels)}, "comments": {"pageInfo": {"hasPreviousPage": False}, "nodes": list(comments)},
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


def run(pr, after, held=None, below=None):
    """One pass, `after` seconds after the head first showed up; held, a
    red-branch fix the branch's watch holds, and why; below, the open PR
    whose head branch is its base."""
    C.NOW = C.ts(T0) + after
    WROTE.clear()
    JOBS.clear()
    SINCE.clear()
    ROLES.clear()
    C.WAKE.clear()
    C.LIMITED.clear()
    POSTED.clear()
    digest = []
    serve_compare(COMPARE[pr["number"]] if pr["number"] in COMPARE else fixture_pages(pr["number"], pr["headRefOid"]))
    touching(pr["number"], pr["headRefOid"], [f["path"] for f in pr["files"]["nodes"]], [x["name"] for x in pr["labels"]["nodes"]])
    H.T.CLASSES.clear()
    with C.acting_for("sam/r"):
        H.handle("sam/r", pr, info, digest, held, below)
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


REAL = {"job": C.job, "stop_job": C.stop_job, "forget_issues": C.forget_issues,
        "onto": H.onto, "carry": V.carry}  # the steward's own, for the checks of what it runs
# What H.onto says: the base's tip a PR's merge would change nothing on, by
# number, or what it raises; any other's changes something on TIP's tip.
ALREADY, TIP = {}, {"v": "f" * 40}


def onto(repo, pr):
    got = ALREADY.get(pr["number"])
    if isinstance(got, Exception):
        raise got
    return (got, True) if got else (TIP["v"], False)


def stand_in():
    """gh, Claude jobs, the queue, the posting, what a merge would change
    (every merge changes something unless ALREADY says) and which earlier
    heads' looks carry (none unless CARRY says), for handle() and the
    passes."""
    H.onto = onto
    C.gh = gh_stub
    C.write = write
    C.job = job
    C.lifecycle = lambda jid: (STATES.get(jid, "running"), HELD.get(jid, []))
    C.stop_job = stop_job
    C.forget_issues = FORGOT.append
    V.queue = queue
    V.post = post
    V.carry = lambda repo, pr, st, names: CARRY.get(pr["headRefOid"])
