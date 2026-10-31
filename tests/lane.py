"""Which gate the shared CI gives a PR, its lane job's own scripts run on real
repos. With pr-gate off (a train repo's), a PR to the default branch gets
none, and every push and a PR to any other branch, a branch whose name
differs only in case included, still get one. With docs-paths, a PR whose
every path is documentation, renames and deletions counted from where the PR
left its base, takes the docs lane: only the checks that read docs, in a job
of its own named so autoland knows; anything under .github/, a .nix file or
a flake.lock never is, whatever the patterns say, and patterns that would
match everything are refused. The lane's check run names the base it chose
against, the docs lane's the commit the PR left that base at too, and a
caller that lists docs runs again when a PR's base changes.

  python3 -I tests/lane.py .github/workflows/flake-check.yml modules/autoland/prs.py .github/workflows/flake.yml
  nix build .#checks.x86_64-linux.lane
"""

import os
import re
import shutil
import subprocess
import sys
import tempfile

import yaml

workflow = yaml.safe_load(open(sys.argv[1]))
jobs = workflow["jobs"]
# PyYAML reads the `on:` key as True.
inputs = workflow[True]["workflow_call"]["inputs"]
assert inputs["pr-gate"]["type"] == "boolean" and inputs["pr-gate"]["default"] is True, inputs["pr-gate"]
for name in ("docs-paths", "docs-checks"):
    assert inputs[name]["type"] == "string" and inputs[name]["default"] == "", inputs[name]

lane = jobs["lane"]
# On every PR, whatever the caller turns on: its check run is how autoland
# knows which base a run judged.
assert lane["if"] == "${{ github.event_name == 'pull_request' }}", lane["if"]
assert lane["outputs"] == {"gate": "${{ steps.base.outputs.lands == 'true' && 'train' || "
                                   "steps.paths.outputs.docs == 'true' && 'docs' || 'full' }}",
                           "fork": "${{ steps.paths.outputs.fork }}"}, lane["outputs"]
# Both check runs are named after what autoland looks for.
names = re.search(r'^LANE, DOCS_LANE = "(.*)", "(.*)"$', open(sys.argv[2]).read(), re.M)
assert names, "autoland names no lane"
assert lane["name"] == names[1] + "${{ github.event.pull_request.base.ref }}", (lane["name"], names[1])
base_step = next(s for s in lane["steps"] if s.get("id") == "base")
assert base_step["env"] == {"PR_GATE": "${{ inputs.pr-gate }}", "BASE": "${{ github.event.pull_request.base.ref }}",
                            "DEFAULT": "${{ github.event.repository.default_branch }}"}, base_step["env"]
checkout = lane["steps"][1]
assert checkout["uses"].startswith("actions/checkout@"), checkout
assert checkout["with"] == {"persist-credentials": False, "ref": "${{ github.event.pull_request.head.sha }}",
                            "fetch-depth": 0, "filter": "blob:none"}, checkout["with"]
paths_step = lane["steps"][2]
assert paths_step["id"] == "paths" and paths_step["shell"] == "bash"
for s in lane["steps"][1:]:
    assert s["if"] == "steps.base.outputs.lands != 'true' && inputs.docs-paths != ''", s
assert paths_step["env"] == {"BASE": "${{ github.event.pull_request.base.ref }}",
                             "HEAD_SHA": "${{ github.event.pull_request.head.sha }}",
                             "DOCS": "${{ inputs.docs-paths }}"}, paths_step["env"]

# The whole gate and the docs lane go by what the lane says (whether each
# runs, case by case, is tests/pr-base.py's). The docs lane is a job of its
# own, under the name autoland looks for.
check, docs = jobs["check"], jobs["docs"]
assert check["needs"] == "lane" and docs["needs"] == "lane"
assert "name" not in check, check.get("name")
assert docs["name"] == names[2] + "${{ github.event.pull_request.base.ref }} from ${{ needs.lane.outputs.fork }}", (
    docs["name"], names[2])
# It reads the tree and attests nothing.
assert docs["permissions"] == {"contents": "read"}, docs["permissions"]
assert not any("attest" in s.get("uses", "") for s in docs["steps"]), docs["steps"]
nix_step = docs["steps"][2]
assert nix_step["if"] == "inputs.install-nix && inputs.docs-checks != ''", nix_step
build = docs["steps"][3]
assert build["env"] == {"CHECKS": "${{ inputs.docs-checks }}"}, build["env"]

# A caller that lists docs runs again on a new base, and only on that edit:
# which paths a PR changes depend on its base.
caller = yaml.safe_load(open(sys.argv[3]))
callers = [j for j in caller["jobs"].values()
           if j.get("uses", "").endswith("/flake-check.yml") and j.get("with", {}).get("docs-paths")]
assert callers, "the caller lists no docs"
assert "edited" in caller[True]["pull_request"]["types"], caller[True]
for j in callers:
    assert j.get("if") == "github.event.action != 'edited' || github.event.changes.base", j.get("if")

tmp = tempfile.mkdtemp()
env = {"PATH": os.environ["PATH"], "HOME": tmp, "GIT_CONFIG_NOSYSTEM": "1"}


def output(script, cwd=tmp, **more):
    """A step's script run as the runner runs it (bash -e, pipefail): its
    exit code, what it printed and what it wrote to GITHUB_OUTPUT."""
    out = os.path.join(tmp, "step-output")
    open(out, "w").close()
    p = subprocess.run(["bash", "--noprofile", "--norc", "-eo", "pipefail", "-c", script], cwd=cwd,
                       capture_output=True, text=True, env={**env, "GITHUB_OUTPUT": out, **more})
    return p.returncode, p.stdout + p.stderr, open(out).read()


def lands(base, default, pr_gate="false"):
    """The base step's word on a PR to base, in a repo whose default branch is default."""
    code, said, out = output(base_step["run"], BASE=base, DEFAULT=default, PR_GATE=pr_gate)
    assert code == 0, said
    return out


# Only the exact default branch is the train's: a case variant is another
# branch, and so is any other name. With pr-gate on, no PR is.
assert lands("main", "main") == "lands=true\n", lands("main", "main")
for other in ("Main", "MAIN", "release", "main2", ""):
    assert lands(other, "main") == "lands=false\n", (other, lands(other, "main"))
assert lands("main", "main", "true") == "lands=false\n"
print("train lands only its default branch's PRs: ok")

GIT = ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"]


def git(*a, cwd):
    return subprocess.run([*GIT, *a], cwd=cwd, env=env, check=True, capture_output=True, text=True).stdout.strip()


def change(cwd, files=(), gone=(), moved=()):
    """A commit writing each of files, deleting each of gone and renaming
    each (from, to) of moved."""
    for f in files:
        os.makedirs(os.path.join(cwd, os.path.dirname(f)), exist_ok=True)
        with open(os.path.join(cwd, f), "a") as fh:
            fh.write(f"{f} changed\n")
        git("add", f, cwd=cwd)
    for f in gone:
        git("rm", "-q", f, cwd=cwd)
    for a, b in moved:
        os.makedirs(os.path.join(cwd, os.path.dirname(b)), exist_ok=True)
        git("mv", a, b, cwd=cwd)
    git("commit", "-qm", "change", "--allow-empty", cwd=cwd)
    return git("rev-parse", "HEAD", cwd=cwd)


# GitHub: main with a bit of everything, the docs included.
origin, work = os.path.join(tmp, "origin.git"), os.path.join(tmp, "work")
git("init", "-q", "--bare", origin, cwd=tmp)
git("init", "-q", work, cwd=tmp)
TREE = ["README.md", "AGENTS.md", "docs/guide.md", "docs/deep/more.md", "src/main.rs", "src/notes.md",
        "archive/old.md", "flake.nix", "flake.lock", "nix/module.nix", "sub/flake.lock", "Cargo.toml",
        ".github/workflows/flake.yml", ".github/pull_request_template.md"]
fork = change(work, TREE)
git("push", "-q", origin, "main", cwd=work)
ALL = "README.md AGENTS.md docs/** **/*.md"


def pr(docs_paths=ALL, files=(), gone=(), moved=(), main_moves=(), name=[0]):
    """The paths step on a PR branch off main that makes the given change,
    after main itself moved on by changing main_moves, as the lane job's
    checkout leaves it: (exit code, what it said, its outputs, the head)."""
    name[0] += 1
    branch = f"pr{name[0]}"
    git("checkout", "-q", "-b", branch, fork, cwd=work)
    head = change(work, files, gone, moved)
    git("push", "-q", origin, branch, cwd=work)
    if main_moves:
        git("checkout", "-q", "main", cwd=work)
        change(work, main_moves)
        git("push", "-q", origin, "main", cwd=work)
    ws = tempfile.mkdtemp(dir=tmp)
    git("clone", "-q", "--no-checkout", origin, ws, cwd=tmp)
    git("checkout", "-q", "--detach", head, cwd=ws)
    code, said, out = output(paths_step["run"], cwd=ws, BASE="main", HEAD_SHA=head, DOCS=docs_paths)
    return code, said, out, head


def lane_of(**kw):
    """Whether the PR takes the docs lane, and what the step said. It names
    the commit the PR left main at, as git has it."""
    code, said, out, head = pr(**kw)
    assert code == 0, said
    fork_now = git("merge-base", "main", head, cwd=origin)
    assert out in (f"fork={fork_now}\ndocs={x}\n" for x in ("true", "false")), (out, fork_now, said)
    return out.endswith("docs=true\n"), said


# Documentation only, as the patterns say: the docs lane. `*` stays in its
# directory and `**` crosses them, the root included.
assert lane_of(files=["README.md"])[0]
assert lane_of(files=["docs/guide.md", "docs/deep/more.md"])[0]
assert lane_of(files=["src/notes.md"])[0]  # **/*.md
assert lane_of(docs_paths="*.md", files=["README.md", "AGENTS.md"])[0]
assert not lane_of(docs_paths="*.md", files=["src/notes.md"])[0]
assert not lane_of(docs_paths="docs/*", files=["docs/deep/more.md"])[0]
# Deleted, renamed or new documentation is still documentation.
assert lane_of(gone=["docs/guide.md"])[0]
assert lane_of(moved=[("docs/guide.md", "docs/renamed.md")])[0]
assert lane_of(files=["docs/new.md"])[0]
# Anything else in the change: the whole gate, which says what isn't documentation.
docs_lane, said = lane_of(files=["README.md", "src/main.rs"])
assert not docs_lane and "src/main.rs" in said and "README.md\n" not in said.partition("aren't documentation:")[2], said
assert not lane_of(files=["Cargo.toml"])[0]
# A rename counts where it started: code moved into docs/ is code leaving.
assert not lane_of(moved=[("src/main.rs", "docs/main.md")])[0]
# A pattern left out with `!` isn't documentation.
assert not lane_of(docs_paths="**/*.md !archive/**", files=["archive/old.md"])[0]
assert lane_of(docs_paths="**/*.md !archive/**", files=["docs/guide.md"])[0]
# What defines the gate never is, whatever the patterns say: .github/ (its
# Markdown too), every .nix file, and any flake.lock.
for f in [".github/workflows/flake.yml", ".github/pull_request_template.md", "flake.nix", "nix/module.nix",
          "flake.lock", "sub/flake.lock"]:
    assert not lane_of(docs_paths="** .github/**", files=[f])[0], f
    assert not lane_of(docs_paths="** .github/**", files=["README.md", f])[0], f
assert not lane_of(docs_paths="**", moved=[(".github/pull_request_template.md", "docs/template.md")])[0]
# Only what the PR changes counts: main moving on with code meanwhile is
# main's, not the PR's.
assert lane_of(files=["README.md"], main_moves=["src/main.rs"])[0]
# A PR that changes nothing gets the whole gate.
assert not lane_of()[0]
print("the docs lane takes documentation only: ok")

# Patterns that would match everything, or that are pathspec magic, are
# refused: the job fails, saying why, rather than guessing.
for bad, why in ((":(top)README.md", "can't start with ':'"), (":!src/**", "can't start with ':'"),
                 ("!src/** !flake.nix", "names no documentation")):
    code, said, out, _ = pr(docs_paths=bad, files=["README.md"])
    assert code != 0 and why in said and "docs=" not in out, (bad, code, said, out)
print("docs-paths that would take everything is refused: ok")

# A head with no history in common with its base gets the whole gate.
ws = tempfile.mkdtemp(dir=tmp)
git("clone", "-q", "--no-checkout", origin, ws, cwd=tmp)
git("checkout", "-q", "--orphan", "alone", cwd=ws)
git("rm", "-rqf", ".", cwd=ws)
alone = change(ws, ["README.md"])
code, said, out = output(paths_step["run"], cwd=ws, BASE="main", HEAD_SHA=alone, DOCS=ALL)
assert code == 0 and out == "docs=false\n" and "shares no history" in said, (code, said, out)
print("unrelated histories get the whole gate: ok")

# The docs lane's build: each check docs-checks names, for this system, and
# nothing when it names none, without asking nix anything.
log = os.path.join(tmp, "nix-log")
bindir = os.path.join(tmp, "bin")
os.mkdir(bindir)
with open(os.path.join(bindir, "nix"), "w") as f:
    f.write(f"""#!{shutil.which("bash")}
echo "$*" >> {log}
[ "$1 $2" = "config show" ] && echo x86_64-linux
exit 0
""")
os.chmod(os.path.join(bindir, "nix"), 0o755)
open(log, "w").close()
code, said, _ = output(build["run"], CHECKS="", PATH=f"{bindir}:{os.environ['PATH']}")
assert code == 0 and "nothing to build" in said and open(log).read() == "", (code, said)
code, said, _ = output(build["run"], CHECKS="lychee  docs", PATH=f"{bindir}:{os.environ['PATH']}")
assert code == 0, said
assert open(log).read().splitlines() == [
    "config show system",
    "build -L --no-link --keep-going .#checks.x86_64-linux.lychee .#checks.x86_64-linux.docs"], open(log).read()
print("the docs lane builds only the docs checks: ok")
