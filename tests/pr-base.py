"""The shared CI's merge step, its own script run on real repos: a PR is
checked merged with its base branch as that branch is when the job starts,
not as it was when the event fired (a run that waited in the queue, or a
re-run), the log names both commits, and a head that no longer merges
cleanly fails the job without anything checked. With pr-gate off (a train
repo's), a PR to the default branch gets no gate, and every push and a PR to
any other branch still do, a branch whose name differs only in case included.
A PR's newer run cancels what its last one still has going, and nothing
else: not a push's run, another PR's, or a sibling call with inputs of its
own; and an old run whose train job was cancelled checks nothing.

  python3 -I tests/pr-base.py .github/workflows/flake-check.yml
  nix build .#checks.x86_64-linux.pr-base
"""

import os
import re
import subprocess
import sys
import tempfile

import yaml

workflow = yaml.safe_load(open(sys.argv[1]))
job = workflow["jobs"]["check"]
# PyYAML reads the `on:` key as True.
pr_gate = workflow[True]["workflow_call"]["inputs"]["pr-gate"]
assert pr_gate["type"] == "boolean" and pr_gate["default"] is True, pr_gate
train = workflow["jobs"]["train"]
assert train.get("if") == "${{ !inputs.pr-gate && github.event_name == 'pull_request' }}", train.get("if")
assert job.get("needs") == "train", job.get("needs")


def text(x):
    """A value as GitHub puts it in a string."""
    return "" if x is None else str(x).lower() if isinstance(x, bool) else str(x)


def value(expr, ctx):
    """An Actions expression (${{ ... }}, or text around some) as GitHub
    works it out, for the little these use: context paths, string literals,
    format(), cancelled(), !, ==, !=, && and || (which, as in Python, give
    an operand's value)."""
    whole = re.fullmatch(r"\$\{\{(.*)\}\}", expr.strip(), re.S)
    if not whole or "${{" in whole[1]:
        return re.sub(r"\$\{\{(.*?)\}\}", lambda m: text(value("${{" + m[1] + "}}", ctx)), expr)
    e = whole[1].replace("cancelled()", "ctx['cancelled()']").replace("format(", "fmt(")
    e = re.sub(r"\b[a-z_][\w-]*(?:\.[\w-]+)+", lambda m: f"ctx[{m[0]!r}]", e)
    e = e.replace("!=", " NE ").replace("!", " not ").replace(" NE ", " != ").replace("&&", " and ").replace("||", " or ")
    return eval(e, {"fmt": lambda f, *a: f.format(*map(text, a)), "ctx": ctx})


# Whether the check job runs: with pr-gate, for every event; without it,
# for anything but a PR the train lands, a train job that failed included
# (fail closed), but not once its train job was cancelled (a newer run of
# the PR has it), nor once the run itself was.
for gate, event, train_result, lands, cancelled, runs in (
        (True, "pull_request", "skipped", None, False, True),
        (True, "push", "skipped", None, False, True),
        (False, "push", "skipped", None, False, True),
        (False, "pull_request", "success", "true", False, False),
        (False, "pull_request", "success", "false", False, True),
        (False, "pull_request", "failure", None, False, True),
        (False, "pull_request", "cancelled", None, False, False),
        (True, "pull_request", "skipped", None, True, False)):
    ctx = {"cancelled()": cancelled, "inputs.pr-gate": gate, "github.event_name": event,
           "needs.train.result": train_result, "needs.train.outputs.lands": lands}
    assert bool(value(job["if"], ctx)) == runs, (gate, event, train_result, lands, cancelled, job["if"])
print("the gate runs where it should: ok")

# Each job's concurrency group: a PR's runs share one, so the newer cancels
# the older; anything else is its run's own; jobs, workflows, PRs and calls
# with other inputs never share one.
INPUTS = {"inputs.runner": "ubuntu-latest", "inputs.install-nix": True, "inputs.eval-apart": False,
          "inputs.pr-gate": True, "inputs.build-skip": "agents"}


def group(name, event="pull_request", pr=12, run=1001, workflow="flake", **inputs):
    c = workflow_jobs[name]["concurrency"]
    assert c["cancel-in-progress"] is True, (name, c)
    ctx = {**INPUTS, **{f"inputs.{k.replace('_', '-')}": v for k, v in inputs.items()}, "github.event_name": event,
           "github.event.pull_request.number": pr if event == "pull_request" else None, "github.run_id": run,
           "github.workflow": workflow}
    return value(c["group"], ctx)


workflow_jobs = workflow["jobs"]
for name in ("train", "check", "commits"):
    assert group(name) == group(name, run=1002), name  # the PR's next run cancels this one
    others = [group(name, pr=13), group(name, workflow="other"), group(name, runner="remi"),
              group(name, build_skip=""), group(name, eval_apart=True), group(name, pr_gate=False),
              group(name, install_nix=False)]
    assert group(name) not in others and len(set(others)) == len(others), (name, others)
assert len({group(n) for n in ("train", "check", "commits")}) == 3
# A push or a train candidate is a run of its own: none cancels another.
assert group("check", "push", run=1001) != group("check", "push", run=1002)
assert group("check", "push", run=12) != group("check", pr=12)
print("only a PR's newer run cancels: ok")
assert train["outputs"] == {"lands": "${{ steps.base.outputs.lands }}"}, train["outputs"]
base_step = next(s for s in train["steps"] if s.get("id") == "base")
assert base_step["shell"] == "bash"
assert base_step["env"] == {"BASE": "${{ github.event.pull_request.base.ref }}",
                            "DEFAULT": "${{ github.event.repository.default_branch }}"}, base_step["env"]
names = [s.get("id") or s.get("uses", "").split("@")[0] for s in job["steps"]]
# The checkout fetches every branch for a PR, then the merge comes before
# the gate checks anything.
assert names[:2] == ["actions/checkout", "merge"] and names.index("gate") > 1, names
checkout = job["steps"][0]["with"]
assert checkout["ref"] == "${{ github.event.pull_request.head.sha }}", checkout
assert checkout["fetch-depth"] == "${{ github.event_name == 'pull_request' && '0' || '1' }}", checkout
assert checkout["persist-credentials"] is False, checkout
step = job["steps"][1]
assert step["if"] == "github.event_name == 'pull_request'", step
assert step["shell"] == "bash"
assert step["env"] == {"BASE": "${{ github.event.pull_request.base.ref }}",
                       "HEAD_SHA": "${{ github.event.pull_request.head.sha }}"}, step["env"]

tmp = tempfile.mkdtemp()


def lands(base, default):
    """The train job's word on a PR to base, in a repo whose default branch is default."""
    out = os.path.join(tmp, "train-output")
    open(out, "w").close()
    subprocess.run(["bash", "-c", base_step["run"]], check=True, stdout=subprocess.DEVNULL,
                   env={"PATH": os.environ["PATH"], "BASE": base, "DEFAULT": default, "GITHUB_OUTPUT": out})
    return open(out).read()


# Only the exact default branch is the train's: a case variant is another
# branch, and so is any other name.
assert lands("main", "main") == "lands=true\n", lands("main", "main")
for other in ("Main", "MAIN", "release", "main2", ""):
    assert lands(other, "main") == "lands=false\n", (other, lands(other, "main"))
print("train lands only its default branch's PRs: ok")

script = os.path.join(tmp, "merge.sh")
open(script, "w").write(step["run"])
env = {"PATH": os.environ["PATH"], "HOME": tmp, "GIT_CONFIG_NOSYSTEM": "1"}
GIT = ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"]


def git(*a, cwd):
    return subprocess.run([*GIT, *a], cwd=cwd, env=env, check=True, capture_output=True, text=True).stdout.strip()


def commit(cwd, f, text):
    open(os.path.join(cwd, f), "w").write(text)
    git("add", f, cwd=cwd)
    git("commit", "-qm", f"{f}: {text.strip()}", cwd=cwd)
    return git("rev-parse", "HEAD", cwd=cwd)


# GitHub: main, and a PR branch off it.
origin, work = os.path.join(tmp, "origin.git"), os.path.join(tmp, "work")
git("init", "-q", "--bare", origin, cwd=tmp)
git("init", "-q", work, cwd=tmp)
event_base = commit(work, "a", "base\n")
git("push", "-q", origin, "main", cwd=work)
git("checkout", "-qb", "feat", cwd=work)
head = commit(work, "b", "feature\n")
git("push", "-q", origin, "feat", cwd=work)


def run():
    """The checkout as the action leaves it for a PR (every branch fetched,
    the head checked out), then the merge step."""
    ws = tempfile.mkdtemp(dir=tmp)
    git("clone", "-q", "--no-checkout", origin, ws, cwd=tmp)
    git("checkout", "-q", "--detach", head, cwd=ws)
    p = subprocess.run(["bash", "-e", "-o", "pipefail", script], cwd=ws, capture_output=True, text=True,
                       env={**env, "BASE": "main", "HEAD_SHA": head})
    return p, ws


# While the run waited, main moved on. It checks the head merged with main as
# main is now, GitHub's parent order, and says so.
git("checkout", "-q", "main", cwd=work)
now = commit(work, "c", "later\n")
git("push", "-q", origin, "main", cwd=work)
assert now != event_base
p, ws = run()
assert p.returncode == 0, p.stderr
assert f"checking {head} merged with main at {now}" in p.stdout, p.stdout
parents = git("cat-file", "commit", "HEAD", cwd=ws).split("\n")
assert [x.split()[1] for x in parents if x.startswith("parent ")] == [now, head], parents
assert open(os.path.join(ws, "c")).read() == "later\n" and open(os.path.join(ws, "b")).read() == "feature\n"
assert git("status", "--porcelain", cwd=ws) == ""

# main moved to something the head conflicts with: the job fails, says why,
# and leaves no merge to check.
commit(work, "b", "other\n")
git("push", "-q", origin, "main", cwd=work)
p, ws = run()
assert p.returncode != 0, p.stdout
assert "::error title=Conflicts with its base::" in p.stdout and head in p.stdout, p.stdout
assert len([x for x in git("cat-file", "commit", "HEAD", cwd=ws).split("\n") if x.startswith("parent ")]) < 2

print("pr base: ok")
