"""What every autoland test stands in for: gh, Claude jobs, GitHub's git
remote and the review agents (each records how it was run, then does what
it's told), and the steward's config and state in a temp dir. A test imports
this first, with the steward as its one argument:

  python3 tests/autoland/review.py modules/autoland/steward.py
  nix build .#checks.x86_64-linux.autoland
"""

import json
import os
import subprocess
import sys
import tempfile

tmp = tempfile.mkdtemp()
os.makedirs(f"{tmp}/state")
os.makedirs(f"{tmp}/cred")
os.makedirs(f"{tmp}/home")
with open(f"{tmp}/cred/deepseek", "w") as f:
    f.write('DEEPSEEK_API_KEY="sk-test-0123456789"\n')

# The review agents: each records how it was run, then does what AGENT says
# (an answer, or an error on stderr and an exit code).
AGENT_LOG, AGENT = f"{tmp}/agent-log", f"{tmp}/agent.json"
os.makedirs(f"{tmp}/agents")
for name in ("codex", "dsh"):
    with open(f"{tmp}/agents/{name}", "w") as f:
        f.write(f"""#!{sys.executable}
import json, os, sys
args = sys.argv[1:]
prompt = sys.stdin.read()
with open({AGENT_LOG!r}, "a") as f:
    f.write(json.dumps({{"agent": {name!r}, "args": args, "cwd": os.getcwd(), "env": dict(os.environ), "prompt": prompt,
                        "overlay": open(args[args.index("--patch") + 1]).read() if "--patch" in args else None}}) + "\\n")
do = json.load(open({AGENT!r})).get({name!r}, {{}})
if "effort" in do and do["effort"] not in json.dumps(args) + (open(args[args.index("--patch") + 1]).read() if "--patch" in args else ""):
    print("dsh: error: thought too long", file=sys.stderr); sys.exit(1)
if do.get("rc"):
    print(do["err"], file=sys.stderr); sys.exit(do["rc"])
out = do.get("say", "Looked.\\n" + json.dumps(do.get("answer", {{"findings": [], "summary": "fine"}})))
if "-o" in args:  # codex writes its last message there, and prints it too
    open(args[args.index("-o") + 1], "w").write(out)
print(out)
""")
    os.chmod(f"{tmp}/agents/{name}", 0o755)


# The sandbox: records how it was set up (and the git config it was given),
# then runs the agent where it says.
BOX_LOG = f"{tmp}/box-log"
with open(f"{tmp}/bwrap", "w") as f:
    f.write(f"""#!{sys.executable}
import json, os, sys
a = sys.argv[1:]
cut = a.index("--")
binds = [(a[i], a[i + 1], a[i + 2]) for i in range(cut) if a[i] in ("--bind", "--ro-bind", "--ro-bind-try")]
git = [open(s).read() for k, s, d in binds if d.endswith("/.gitconfig")]
with open({BOX_LOG!r}, "a") as f:
    f.write(json.dumps({{"args": a[:cut], "binds": binds, "gitconfig": git}}) + "\\n")
os.chdir(a[a.index("--chdir") + 1])
os.execv(a[cut + 1], a[cut + 1:])
""")
os.chmod(f"{tmp}/bwrap", 0o755)


def boxes():
    runs = [json.loads(x) for x in open(BOX_LOG)]
    open(BOX_LOG, "w").close()
    return runs


def agent(**do):
    with open(AGENT, "w") as f:
        json.dump(do, f)
    open(AGENT_LOG, "w").close()


def agent_runs():
    return [json.loads(x) for x in open(AGENT_LOG)]


agent()
with open(f"{tmp}/config.json", "w") as f:
    json.dump({
        "owners": ["sam"], "exclude": [], "context": "review-settled", "rulesets": True, "keepBranches": [],
        "baselineJobs": False, "baselinePerPass": 1, "authors": ["sam"],
        "stateDir": f"{tmp}/state", "projectsDir": f"{tmp}/none", "renovateEmail": "bot@renovateapp.com", "proj": "proj", "owner": "Sam", "cleanBranches": False,
        "labels": {"hold": "hold", "owner": "needs-owner"}, "notify": {"repo": "", "assignee": "", "label": "autoland"},
        "defaults": {"rounds": 5,
                     # autoland.nix's default.
                     "oneWay": ["(^|/)CODEOWNERS$", "(^|/)secrets?/", "(^|/)\\.sops\\.yaml$",
                                "\\.sops\\.(ya?ml|json|env)$", "(^|/)migrations?/"], "handoffAfter": 30, "settleDelay": 3,
                     "directPush": False, "ruleset": True, "requireCi": True,
                     "proofJobs": {}, "codex": True, "deepseek": True},
        "repos": {},
        "reviewers": {"codex": {"enable": True, "effort": "high", "parallel": 2},
                      "deepseek": {"enable": True, "effort": "max", "parallel": 2}},
        "skipPaths": ["(^|/)secrets?/", r"\.(pem|key|p12|pfx)$", r"(^|/)\.env(\..*)?$"],
        "agents": f"{tmp}/agents",
        "sandbox": f"{tmp}/bwrap",
    }, f)
os.environ["AUTOLAND_CONFIG"] = f"{tmp}/config.json"
os.environ["CREDENTIALS_DIRECTORY"] = f"{tmp}/cred"
os.environ["HOME"] = f"{tmp}/home"
os.environ["GH_TOKEN"] = "ghp_steward_token"  # never passed on to an agent

sys.path.insert(0, os.path.dirname(os.path.abspath(sys.argv[1])))
sys.argv = ["steward"]
import common as C  # noqa: E402,F401
import repos as R  # noqa: E402,F401
import review_worker as W  # noqa: E402,F401
import reviewers as V  # noqa: E402,F401
import steward as S  # noqa: E402,F401

for d in (V.QUEUE, V.SNAP, V.DONE, V.GIT):
    os.makedirs(f"{tmp}/state/{d}")


def sh(*a, cwd=None):
    return subprocess.run(a, cwd=cwd, check=True, capture_output=True, text=True).stdout
