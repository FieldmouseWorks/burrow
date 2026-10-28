"""house-sync with stand-ins for gh and the notifier: which repos a pass reads
and updates, and when the owner hears about it.

    python3 -I tests/house-sync.py modules/house/sync.sh house

The gh stand-in answers from a fake GitHub (repos, their AGENTS.md, open PRs)
and runs --jq through jq, as gh does, so the script's own filters are tested.
"""

import base64
import fcntl
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile

SYNC = open(sys.argv[1]).read()
HOUSE = os.path.abspath(sys.argv[2])
blocks = {f[:-3]: open(os.path.join(HOUSE, f)).read() for f in sorted(os.listdir(HOUSE)) if f.endswith(".md")}
tmp = tempfile.mkdtemp()

# burrow's own blocks: each file is one, named for it, its marker lines first
# and last, and none of its other lines looks like a marker (house-sync would
# take it for a broken block).
assert set(blocks) == {"rules", "prs"}, sorted(os.listdir(HOUSE))
for name, text in blocks.items():
    lines = text.splitlines()
    assert re.fullmatch(r"[a-z]+", name), name
    assert re.match(rf"<!-- house:{name} v[0-9]+ begin\. .* -->$", lines[0]), (name, lines[0])
    assert lines[-1] == f"<!-- house:{name} end -->", (name, lines[-1])
    assert not any(x.startswith("<!-- house:") for x in lines[1:-1]), name
rules, prs = blocks["rules"], blocks["prs"]
print("blocks: ok")
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


CURRENT = f"# AGENTS.md\n\n{rules}\n## Own rules\n\n{prs}\nOwn PR notes.\n"
STALE = CURRENT.replace("These hold in every repo.", "These held in every repo.")
STALE_PRS = CURRENT.replace("never as drafts", "as drafts")
assert STALE != CURRENT and STALE_PRS != CURRENT


LOCK = os.path.join(tmp, "lock")


def start(repos, *args, owners="Sam", exclude="", current=HOUSE):
    """Start one pass, with the system's blocks at `current`."""
    for f in ("gh.log", "notify.log"):
        open(os.path.join(tmp, f), "w").close()
    json.dump({"repos": {r["full_name"]: r for r in repos}}, open(os.path.join(tmp, "github.json"), "w"))
    env = {"HOUSE": HOUSE, "CURRENT": current, "LOCK": LOCK, "OWNERS": owners, "EXCLUDE": exclude, "NOTIFY": notify}
    script = "".join(f"{k}={shlex.quote(v)}\n" for k, v in env.items()) + SYNC
    return subprocess.Popen(["bash", "-c", script, "house-sync", *args], stdout=subprocess.PIPE,
                            stderr=subprocess.STDOUT, text=True,
                            env={**os.environ, "PATH": bin_ + ":" + os.environ["PATH"]})


def sync(repos, *args, ok=True, **kw):
    """Run one pass; return (the repos it wrote to, the gh calls, notices)."""
    run = start(repos, *args, **kw)
    out, _ = run.communicate()
    sys.stdout.write(out)
    assert (run.returncode == 0) == ok, (args, run.returncode)
    calls = [json.loads(x) for x in open(os.path.join(tmp, "gh.log"))]
    notices = [json.loads(x)[:2] for x in open(os.path.join(tmp, "notify.log"))]
    wrote = sorted({c[c.index("-R") + 1] for c in calls if c[:2] == ["pr", "create"]})
    return wrote, calls, notices


def written(calls):
    """The AGENTS.md each PR the pass opened writes, by repo."""
    out = {}
    for c in calls:
        if c[:3] == ["api", "-X", "PUT"]:
            repo = c[3].split("/contents/")[0][len("repos/"):]
            content = next(x for x in c if x.startswith("content="))[len("content="):]
            out[repo] = base64.b64decode(content).decode()
    return out


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

# Each block on its own: a stale Pull requests block gets a PR like stale
# rules do, and the PR writes every block as burrow has it, everything outside
# the markers as it was. A repo without one of the blocks yet gets the others
# kept, and nothing added. An empty block is a block too.
empty_prs = CURRENT.replace(prs, prs.splitlines(keepends=True)[0] + prs.splitlines(keepends=True)[-1])
for agents in (STALE_PRS, STALE, STALE.replace("never as drafts", "as drafts"), empty_prs):
    wrote, calls, notices = sync([gh_repo("sam/a", agents)], "sam/a")
    assert wrote == ["sam/a"], wrote
    assert written(calls) == {"sam/a": CURRENT}, written(calls)
only_rules = f"# AGENTS.md\n\n{rules}\n## Pull requests\n\nOur own.\n"
for agents, want in ((only_rules, []), (only_rules.replace("These hold", "These held"), ["sam/a"])):
    wrote, calls, notices = sync([gh_repo("sam/a", agents)], "sam/a")
    assert wrote == want, (want, wrote)
    if want:
        assert written(calls) == {"sam/a": only_rules}, written(calls)
only_prs = f"# AGENTS.md\n\n{prs.replace('never as drafts', 'as drafts')}"
wrote, calls, notices = sync([gh_repo("sam/a", only_prs)], "sam/a")
assert written(calls) == {"sam/a": f"# AGENTS.md\n\n{prs}"}, written(calls)
print("blocks each: ok")

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
# Broken markers: a block twice, a begin with no end, or one block's markers
# inside another's (either way round). The repo gets no PR, and the owner a
# notice from the full pass.
rules_lines = rules.splitlines(keepends=True)
for agents in (STALE + rules, STALE_PRS + prs, CURRENT.replace("<!-- house:prs end -->\n", ""),
               f"# AGENTS.md\n\n{rules_lines[0]}{prs}{''.join(rules_lines[1:])}",
               f"# AGENTS.md\n\n{prs.splitlines(keepends=True)[0]}{rules}<!-- house:prs end -->\n"):
    mangled = [gh_repo("sam/a", agents)]
    wrote, calls, notices = sync(mangled, "sam/a")
    assert wrote == [] and notices == [], (agents, wrote, notices)
    wrote, calls, notices = sync(mangled)
    assert wrote == [] and notices == [["open", "house-markers"], ["close", "house-sync"]], (agents, notices)
print("failures: ok")

# status names a repo too, and changes nothing.
wrote, calls, notices = sync(everyone, "status", "sam/a")
assert wrote == [] and read(calls) == ["sam/a"] and notices == [], (wrote, calls, notices)
assert not any(c[:2] == ["api", "-X"] or c[1] == "repos/sam/a/git/refs" for c in calls), calls
print("status: ok")

# One pass at a time: while another holds the lock, a pass waits, full or one
# repo's, and starts once it's free.
for args in ((), ("sam/a",)):
    held = open(LOCK, "w")
    fcntl.flock(held, fcntl.LOCK_EX)
    run = start(everyone, *args)
    try:
        run.wait(timeout=2)
        raise AssertionError(f"{args}: ran while another pass held the lock")
    except subprocess.TimeoutExpired:
        pass
    assert open(os.path.join(tmp, "gh.log")).read() == "", args
    held.close()
    out, _ = run.communicate(timeout=30)
    sys.stdout.write(out)
    assert run.returncode == 0, (args, run.returncode)
    assert any(json.loads(c)[:2] == ["pr", "create"] for c in open(os.path.join(tmp, "gh.log"))), args
print("lock: ok")

# A pass whose blocks aren't the system's any more (it began, or waited for
# the lock, across a switch) does nothing, so it can't close the new blocks'
# PR. Any block counts, and so does one more or one fewer.
def other(change):
    # Written out, not copied: a copy of the store's would be read-only.
    d = tempfile.mkdtemp(dir=tmp)
    for name, text in blocks.items():
        open(os.path.join(d, f"{name}.md"), "w").write(text)
    change(d)
    return d


for current in (other(lambda d: open(os.path.join(d, "prs.md"), "a").write("More.\n")),
                other(lambda d: open(os.path.join(d, "new.md"), "w").write("<!-- house:new v1 begin. -->\n<!-- house:new end -->\n")),
                other(lambda d: os.remove(os.path.join(d, "prs.md"))),
                os.path.join(tmp, "missing")):
    for args in ((), ("sam/a",)):
        wrote, calls, notices = sync(everyone + [gh_repo("sam/n", STALE, prs=[{"number": 7, "headRefName": "house/rules-0ther"}])],
                                     *args, current=current)
        assert calls == [] and notices == [], (current, args, calls, notices)
print("superseded: ok")

# A PR for older blocks is closed once a newer one is open, or once the
# default branch has the current blocks.
# The current one, already open, stays, and so does anyone else's PR.
rev = subprocess.run("sha256sum -- *.md | sha256sum | cut -c1-8", shell=True, cwd=HOUSE,
                     capture_output=True, text=True, check=True).stdout.strip()
older = {"number": 7, "headRefName": "house/rules-0ther"}
current = {"number": 8, "headRefName": f"house/rules-{rev}", "url": "https://github.com/sam/a/pull/8"}
theirs = {"number": 9, "headRefName": "house-cleaning"}
for agents, open_prs in ((STALE_PRS, [older, theirs]), (CURRENT, [older, theirs]), (STALE_PRS, [older, current, theirs])):
    wrote, calls, notices = sync([gh_repo("sam/a", agents, prs=open_prs)], "sam/a")
    assert [c[:3] for c in calls if c[:2] == ["pr", "close"]] == [["pr", "close", "7"]], (agents, calls)
    assert wrote == (["sam/a"] if agents == STALE_PRS and current not in open_prs else []), (agents, wrote)
print("older PRs: ok")
