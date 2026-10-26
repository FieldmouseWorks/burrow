"""house-sync with stand-ins for gh and the notifier: which repos a pass reads
and updates, and when the owner hears about it.

    python3 -I tests/house-sync.py modules/house/sync.sh house/rules.md

The gh stand-in answers from a fake GitHub (repos, their AGENTS.md, open PRs)
and runs --jq through jq, as gh does, so the script's own filters are tested.
"""

import json
import os
import shlex
import subprocess
import sys
import tempfile

SYNC = open(sys.argv[1]).read()
RULES = os.path.abspath(sys.argv[2])
rules = open(RULES).read()
tmp = tempfile.mkdtemp()
bin_ = os.path.join(tmp, "bin")
os.makedirs(bin_)


def stub(name, body):
    path = os.path.join(bin_, name)
    with open(path, "w") as f:
        f.write(f"#!{sys.executable} -I\nimport base64, json, os, subprocess, sys\nSTUB = {tmp!r}\n{body}")
    os.chmod(path, 0o755)
    return path


# GitHub as gh shows it, from github.json: {"repos": {name: {...}}}. Every
# call goes to gh.log; --jq runs the answer through jq -r.
stub("gh", """a = sys.argv[1:]
open(os.path.join(STUB, "gh.log"), "a").write(json.dumps(a) + "\\n")
gh = json.load(open(os.path.join(STUB, "github.json")))
jq = a[a.index("--jq") + 1] if "--jq" in a else None
def answer(v):
    out = json.dumps(v)
    if jq is not None:
        out = subprocess.run(["jq", "-r", jq], input=out, capture_output=True, text=True, check=True).stdout
    sys.stdout.write(out)
    sys.exit(0)
def fail(code):
    sys.stderr.write(f"HTTP {code}\\n")
    sys.exit(1)
def repo(name):
    for r in gh["repos"].values():
        if r["full_name"].lower() == name.lower():
            return r
    fail(404)
if a[:2] == ["repo", "list"]:
    owner = a[2].lower()
    answer([{"nameWithOwner": r["full_name"], "defaultBranchRef": {"name": r["default_branch"]}}
            for r in gh["repos"].values()
            if r["owner"]["login"].lower() == owner and not r["archived"] and not r["fork"]])
if a[:2] == ["pr", "list"]:
    r = repo(a[a.index("-R") + 1])
    prs = r.get("prs", [])
    if "--head" in a:
        prs = [p for p in prs if p["headRefName"] == a[a.index("--head") + 1]]
    answer(prs)
if a[:2] == ["pr", "create"]:
    print(f"https://github.com/{a[a.index('-R') + 1]}/pull/9")
    sys.exit(0)
if a[:2] == ["pr", "close"]:
    sys.exit(0)
if a[0] == "api":
    rest = a[1:]
    while rest[0].startswith("-"):
        rest = rest[2:]
    path = rest[0]
    parts = path.split("?")[0].split("/")
    r = repo("/".join(parts[1:3]))
    if r["fails"] == "all" or (r["fails"] and len(parts) > 3):
        fail(500)
    if len(parts) == 3:
        answer(r)
    if parts[3:5] == ["git", "ref"]:
        answer({"object": {"sha": "c0ffee"}})
    if parts[3:5] == ["git", "refs"] or "-X" in a:
        answer({})
    if parts[3:5] == ["contents", "AGENTS.md"]:
        if "agents" not in r:
            fail(404)
        answer({"sha": "f11e", "content": base64.b64encode(r["agents"].encode()).decode()})
fail(400)
""")
notify = stub("notify", """open(os.path.join(STUB, "notify.log"), "a").write(json.dumps(sys.argv[1:]) + "\\n")""")


def gh_repo(name, agents=None, archived=False, fork=False, fails=False, prs=()):
    owner = name.split("/")[0]
    r = {"full_name": name, "default_branch": "main", "owner": {"login": owner},
         "archived": archived, "fork": fork, "fails": fails, "prs": list(prs)}
    if agents is not None:
        r["agents"] = agents
    return r


CURRENT = f"# AGENTS.md\n\n{rules}\n## Own rules\n"
STALE = CURRENT.replace("These hold in every repo.", "These held in every repo.")
assert STALE != CURRENT


def sync(repos, *args, owners="Sam", exclude="", ok=True):
    """Run one pass; return (the repos it wrote to, the gh calls, notices)."""
    for f in ("gh.log", "notify.log"):
        open(os.path.join(tmp, f), "w").close()
    json.dump({"repos": {r["full_name"]: r for r in repos}}, open(os.path.join(tmp, "github.json"), "w"))
    env = {"RULES": RULES, "OWNERS": owners, "EXCLUDE": exclude, "NOTIFY": notify}
    script = "".join(f"{k}={shlex.quote(v)}\n" for k, v in env.items()) + SYNC
    run = subprocess.run(["bash", "-c", script, "house-sync", *args], capture_output=True, text=True, check=False,
                         env={**os.environ, "PATH": bin_ + ":" + os.environ["PATH"]})
    sys.stdout.write(run.stdout + run.stderr)
    assert (run.returncode == 0) == ok, (args, run.returncode)
    calls = [json.loads(x) for x in open(os.path.join(tmp, "gh.log"))]
    notices = [json.loads(x)[:2] for x in open(os.path.join(tmp, "notify.log"))]
    wrote = sorted({c[c.index("-R") + 1] for c in calls if c[:2] == ["pr", "create"]})
    return wrote, calls, notices


def read(calls):
    """The repos whose AGENTS.md the pass read."""
    return sorted({c[1].split("/contents/")[0][len("repos/"):] for c in calls
                   if c[0] == "api" and "/contents/AGENTS.md?" in c[1]})


everyone = [gh_repo("sam/a", STALE), gh_repo("sam/b", CURRENT), gh_repo("sam/old", STALE, archived=True),
            gh_repo("sam/copy", STALE, fork=True), gh_repo("eve/x", STALE)]
ALL_CLEAR = [["close", "house-markers"], ["close", "house-sync"]]

# The full pass: every repo of the owners', and the owner's notices.
wrote, calls, notices = sync(everyone)
assert wrote == ["sam/a"], wrote
assert read(calls) == ["sam/a", "sam/b"], read(calls)
assert notices == ALL_CLEAR, notices
print("full pass: ok")

# One repo's pass (a push to its default branch): that repo alone, no listing,
# and the notices left to the full pass.
for name in ("sam/a", "SAM/A"):
    wrote, calls, notices = sync(everyone, name)
    assert wrote == ["sam/a"], (name, wrote)
    assert read(calls) == ["sam/a"], (name, read(calls))
    assert not any(c[:2] == ["repo", "list"] for c in calls), calls
    assert notices == [], notices
wrote, calls, notices = sync(everyone, "sam/b")
assert wrote == [] and read(calls) == ["sam/b"] and notices == [], (wrote, calls, notices)
print("one repo: ok")

# Only a repo a full pass would keep: not another owner's, an archived one, a
# fork or an excluded one.
for name, exclude in (("eve/x", ""), ("sam/old", ""), ("sam/copy", ""), ("sam/a", "sam/*")):
    wrote, calls, notices = sync(everyone, name, exclude=exclude)
    assert wrote == [] and read(calls) == [] and notices == [], (name, wrote, calls, notices)
print("repos it doesn't keep: ok")

# One repo that fails (it can't be read, or its AGENTS.md can't) fails the
# pass, and tells the owner nothing: the next full pass does.
for fails in ("all", "contents"):
    wrote, calls, notices = sync([gh_repo("sam/a", STALE, fails=fails)], "sam/a", ok=False)
    assert wrote == [] and notices == [], (fails, wrote, notices)
broken = [gh_repo("sam/a", STALE, fails="contents"), gh_repo("sam/b", STALE)]
wrote, calls, notices = sync(broken)
assert wrote == ["sam/b"], wrote
assert notices == [["open", "house-sync"]], notices
mangled = [gh_repo("sam/a", STALE + rules)]
wrote, calls, notices = sync(mangled, "sam/a")
assert wrote == [] and notices == [], (wrote, notices)
wrote, calls, notices = sync(mangled)
assert notices == [["open", "house-markers"], ["close", "house-sync"]], notices
print("failures: ok")

# status names a repo too, and changes nothing.
wrote, calls, notices = sync(everyone, "status", "sam/a")
assert wrote == [] and read(calls) == ["sam/a"] and notices == [], (wrote, calls, notices)
assert not any(c[:2] == ["api", "-X"] or c[1] == "repos/sam/a/git/refs" for c in calls), calls
print("status: ok")
