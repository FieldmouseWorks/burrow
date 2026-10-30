"""The shared rust-first check, its own script run with gh stood in for: a
PR that adds Python, or grows the Python it had past the line a fix stays
under, fails unless its body has a "Why not Rust:" line with a reason; one
that doesn't passes whatever its body says. And every caller runs it again
when the body is edited.

  python3 -I tests/rust-first.py .github/workflows/rust-first-check.yml \\
    .github/workflows/rust-first.yml templates/project/.github/workflows/rust-first.yml
  nix build .#checks.x86_64-linux.rust-first
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile

import yaml

workflow = yaml.safe_load(open(sys.argv[1]))
job = workflow["jobs"]["python"]
assert job["if"] == "github.event_name == 'pull_request'", job["if"]
assert job["permissions"] == {"contents": "read", "pull-requests": "read"}, job["permissions"]
growth_default = workflow[True]["workflow_call"]["inputs"]["growth"]["default"]
step = next(s for s in job["steps"] if s.get("id") == "python")
assert step["shell"] == "bash"
assert step["env"]["NUMBER"] == "${{ github.event.pull_request.number }}", step["env"]
assert step["env"]["GROWTH"] == "${{ inputs.growth }}", step["env"]

# Every caller: on each push and on an edited body, with what the check reads.
for caller in sys.argv[2:]:
    c = yaml.safe_load(open(caller))
    on = c[True]["pull_request"]
    assert set(on["types"]) == {"opened", "edited", "reopened", "synchronize"}, (caller, on)
    (called,) = c["jobs"].values()
    assert called["uses"].split("@")[0].endswith(".github/workflows/rust-first-check.yml"), (caller, called)
    assert called["permissions"] == {"contents": "read", "pull-requests": "read"}, (caller, called)

tmp = tempfile.mkdtemp()
script = os.path.join(tmp, "python.sh")
open(script, "w").write(step["run"])

# gh as the job calls it: the PR, its files (--paginate, --jq), and a file's
# raw content at a commit, answered from files under tmp; every call logged.
bin_dir = os.path.join(tmp, "bin")
os.mkdir(bin_dir)
gh = os.path.join(bin_dir, "gh")
open(gh, "w").write("#!" + shutil.which("bash") + r"""
echo "$*" >> "$GH_LOG"
[ -z "$GH_FAIL" ] || exit 1
path= expr=
while [ $# -gt 0 ]; do
  case $1 in
    api | --paginate) ;;
    --jq) expr=$2; shift ;;
    -H) [ "$2" = 'Accept: application/vnd.github.raw' ] || exit 2; shift ;;
    *) path=$1 ;;
  esac
  shift
done
case $path in
  */files\?per_page=100) f=$FILES ;;
  */contents/*)
    p=${path#*/contents/}
    raw=${p%%\?ref=*}
    f=$TREES/${p##*\?ref=}/$(printf '%b' "$(sed 's/%/\\x/g' <<<"$raw")")
    [ -f "$f" ] || exit 1
    ;;
  repos/sam/r/pulls/7) f=$PR ;;
  *) exit 3 ;;
esac
if [ -n "$expr" ]; then exec jq -r "$expr" "$f"; fi
cat "$f"
""")
os.chmod(gh, 0o755)


def change(status, filename, added=0, removed=0, previous=None):
    f = {"status": status, "filename": filename, "additions": added, "deletions": removed}
    if previous:
        f["previous_filename"] = previous
    return f


def run(files, body="", trees=None, growth=growth_default, fail=False):
    pr = os.path.join(tmp, "pr.json")
    json.dump({"body": body, "head": {"sha": "4ead"}, "base": {"sha": "ba5e"}}, open(pr, "w"))
    listed = os.path.join(tmp, "files.json")
    json.dump(files, open(listed, "w"))
    tree_dir = os.path.join(tmp, "trees")
    shutil.rmtree(tree_dir, ignore_errors=True)
    for sha, contents in (trees or {}).items():
        for path, text in contents.items():
            full = os.path.join(tree_dir, sha, path)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            open(full, "w").write(text)
    log = os.path.join(tmp, "gh.log")
    if os.path.exists(log):
        os.remove(log)
    runner_temp = os.path.join(tmp, "runner")
    shutil.rmtree(runner_temp, ignore_errors=True)
    os.mkdir(runner_temp)
    env = {"PATH": bin_dir + os.pathsep + os.environ["PATH"], "PR": pr, "FILES": listed, "TREES": tree_dir,
           "GH_LOG": log, "GH_FAIL": "1" if fail else "", "GH_TOKEN": "t", "GITHUB_REPOSITORY": "sam/r",
           "NUMBER": "7", "GROWTH": str(growth), "RUNNER_TEMP": runner_temp}
    # GitHub runs `shell: bash` as this.
    p = subprocess.run([shutil.which("bash"), "--noprofile", "--norc", "-eo", "pipefail", script],
                       env=env, capture_output=True, text=True)
    calls = open(log).read().splitlines() if os.path.exists(log) else []
    return p.returncode, p.stdout + p.stderr, calls


def fails(files, body="", **kw):
    rc, out, _ = run(files, body, **kw)
    errors = [line for line in out.splitlines() if line.startswith("::error")]
    assert rc == 1 and len(errors) == 1 and "Why not Rust:" in errors[0], (files, body, rc, out)
    return errors[0]


def passes(files, body="", **kw):
    rc, out, calls = run(files, body, **kw)
    assert rc == 0 and "::error" not in out, (files, body, rc, out)
    return out, calls


REASON = "Why not Rust: a throwaway script for one migration."

# No Python in the change: it passes, body or none, and reads the PR as it is
# now, every page of its files.
_, calls = passes([change("modified", "src/main.rs", 300, 10), change("added", "docs/x.md", 40)])
assert calls[0] == "api repos/sam/r/pulls/7", calls
assert calls[1].startswith("api --paginate repos/sam/r/pulls/7/files?per_page=100 --jq "), calls
print("no Python: ok")

# The case it's for: a new Python file and no reason. It fails, and names
# neither the files nor anything from the body in the error.
error = fails([change("added", "tools/new_tool.py", 120)], "Adds a tool.\n\n::warning::from the body")
assert "new_tool" not in error and "from the body" not in error, error
rc, out, _ = run([change("added", "tools/new_tool.py", 120)], "::warning::from the body")
assert "::warning::from the body" not in out, out
print("a new Python file without a reason fails: ok")

# With the line it passes, however the line is written: in the middle of the
# body, in any case, bold, in a list, after a CRLF from GitHub's editor.
for body in (REASON, f"Adds a tool.\n\n{REASON}\n\nRefs #1", "why not rust: tuning it still",
             "**Why not Rust:** pytest is the harness here", "- Why not Rust: one-off", "x\r\nWhy not Rust: one-off\r\n"):
    passes([change("added", "tools/new_tool.py", 120)], body)
# But not with nothing after it, or the words somewhere else in a line.
for body in ("Why not Rust:", "Why not Rust: **", "Why not Rust:\r\nnext line", "We said why not Rust: no",
             "Why not Rust"):
    fails([change("added", "tools/new_tool.py", 120)], body)
print("the line, and only the line with a reason: ok")

# Stubs, a file copied to a new one, and one renamed from something else
# into Python are new Python too.
fails([change("added", "pkg/types.pyi", 10)])
fails([change("copied", "pkg/b.py", 0, previous="pkg/a.py")])
fails([change("renamed", "tools/run.py", 3, 1, previous="tools/run.sh")])
# A Python file renamed within Python is growth, like an edit.
passes([change("renamed", "tools/b.py", 5, 2, previous="tools/a.py")])
print("stubs, copies and renames into Python: ok")

# Growth: lines added to the Python it had, net of all it takes out. Up to
# the limit is a fix; past it needs the reason; across files it adds up.
passes([change("modified", "m/a.py", growth_default, 0)])
fails([change("modified", "m/a.py", growth_default + 1, 0)])
fails([change("modified", "m/a.py", 30, 0), change("modified", "m/b.py", 30, 5)])
passes([change("modified", "m/a.py", 30, 0), change("modified", "m/b.py", 30, 10)])
# Lines taken out offset it, a deleted file's whole length included, so a
# port that patches a caller as it deletes what it ported passes.
passes([change("modified", "m/a.py", 200, 0), change("removed", "m/old.py", 0, 400)])
passes([change("modified", "m/a.py", growth_default + 1, 0)], REASON)
# A repo can set its own limit.
fails([change("modified", "m/a.py", 11, 0)], growth=10)
print("growth past the limit, net of what goes: ok")

# A file with no extension is Python by its shebang at the head; a removed
# one by its shebang at the base. Others aren't, and aren't read.
fails([change("added", "bin/wend", 80)], trees={"4ead": {"bin/wend": "#!/usr/bin/env python3\nprint()\n"}})
passes([change("added", "bin/tool", 80)], trees={"4ead": {"bin/tool": "#!/usr/bin/env bash\necho\n"}})
passes([change("modified", "bin/wend", 200, 0), change("removed", "bin/old", 0, 300)],
       trees={"4ead": {"bin/wend": "#!/usr/bin/python\n"}, "ba5e": {"bin/old": "#!/usr/bin/python3\n"}})
_, calls = passes([change("added", "README.md", 80), change("added", "x/y.sh", 9)])
assert not any("/contents/" in c for c in calls), calls
# Paths are sent whole and escaped: a space, a percent sign.
fails([change("added", "my dir/run 100%", 5)], trees={"4ead": {"my dir/run 100%": "#! /usr/bin/env python\n"}})
print("Python by its shebang: ok")

# GitHub not answering fails it, a file's content included: no answer is no
# verdict, never "not Python".
rc, out, _ = run([change("added", "tools/new_tool.py", 120)], REASON, fail=True)
assert rc != 0, out
rc, out, _ = run([change("added", "bin/wend", 80)], REASON, trees={})
assert rc != 0 and "the body says" not in out, out
print("no answer from GitHub: ok")
