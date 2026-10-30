"""The shared CI's merge step, its own script run on real repos: a PR is
checked merged with its base branch as that branch is when the job starts,
not as it was when the event fired (a run that waited in the queue, or a
re-run), the log names both commits, and a head that no longer merges
cleanly fails the job without anything checked. With pr-gate off (a train
repo's), a PR to the default branch gets no gate, and every push and a PR to
any other branch still do, a branch whose name differs only in case included.

  python3 -I tests/pr-base.py .github/workflows/flake-check.yml
  nix build .#checks.x86_64-linux.pr-base
"""

import os
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
assert job.get("if") == ("${{ !cancelled() && (inputs.pr-gate || github.event_name != 'pull_request' ||"
                         " needs.train.outputs.lands != 'true') }}"), job.get("if")
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
