"""The shared rust-first check, its own script run with gh stood in for: a
PR that adds Python, or grows the Python it had past the line a fix stays
under, fails unless its body has a "Why not Rust:" line with a reason; one
that doesn't passes whatever its body says. The verdict is a commit status
for the PR on the head the event names, which a newer run replaces, posted
again whenever the PR changed while it was judged, so the last word is for
the PR as it is; a fork's PR, which can't post one, gets it as the job's
result. And every caller runs it
again when the body is edited, with a token that can post the status.

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
PERMISSIONS = {"contents": "read", "pull-requests": "read", "statuses": "write"}
assert job["permissions"] == PERMISSIONS, job["permissions"]
growth_default = workflow[True]["workflow_call"]["inputs"]["growth"]["default"]
step = next(s for s in job["steps"] if s.get("id") == "python")
assert step["shell"] == "bash"
assert step["env"]["NUMBER"] == "${{ github.event.pull_request.number }}", step["env"]
assert step["env"]["HEAD_SHA"] == "${{ github.event.pull_request.head.sha }}", step["env"]
assert step["env"]["GROWTH"] == "${{ inputs.growth }}", step["env"]

# Every caller: on each push and on an edited body, with what the check reads.
for caller in sys.argv[2:]:
    c = yaml.safe_load(open(caller))
    on = c[True]["pull_request"]
    assert set(on["types"]) == {"opened", "edited", "reopened", "synchronize"}, (caller, on)
    (called,) = c["jobs"].values()
    assert called["uses"].split("@")[0].endswith(".github/workflows/rust-first-check.yml"), (caller, called)
    assert called["permissions"] == PERMISSIONS, (caller, called)

tmp = tempfile.mkdtemp()
script = os.path.join(tmp, "python.sh")
open(script, "w").write(step["run"])

# gh as the job calls it: the PR, the comparison of two commits (its merge
# base and files), a file's raw content at a commit, answered from files
# under tmp, and nothing else (not the PR's own files, which follow its head
# as it moves); and the statuses it posts, kept as JSON lines. Before its Nth
# post, what CHANGES/N holds happens: the PR changes, and other runs post
# theirs. RESTLESS: the PR's body changes every time it's read. Every call
# logged.
bin_dir = os.path.join(tmp, "bin")
os.mkdir(bin_dir)
gh = os.path.join(bin_dir, "gh")
open(gh, "w").write("#!" + shutil.which("bash") + r"""
echo "$*" >> "$GH_LOG"
[ -z "$GH_FAIL" ] || exit 1
path= expr= method=GET fields=()
while [ $# -gt 0 ]; do
  case $1 in
    api) ;;
    --jq) expr=$2; shift ;;
    -H) [ "$2" = 'Accept: application/vnd.github.raw' ] || exit 2; shift ;;
    -X) method=$2; shift ;;
    -f) fields+=("$2"); shift ;;
    *) path=$1 ;;
  esac
  shift
done
if [ "$method" != GET ] || [ ${#fields[@]} -gt 0 ]; then
  case "$method $path" in 'POST repos/sam/r/statuses/'*) ;; *) exit 4 ;; esac
  n=$(($(cat "$POSTS" 2>/dev/null || echo 0) + 1))
  echo "$n" > "$POSTS"
  if [ -d "$CHANGES/$n" ]; then
    [ ! -f "$CHANGES/$n/pr.json" ] || cp "$CHANGES/$n/pr.json" "$PR"
    [ ! -f "$CHANGES/$n/statuses" ] || cat "$CHANGES/$n/statuses" >> "$STATUSES"
  fi
  exec jq -cn --arg sha "${path##*/}" \
    '$ARGS.positional | map(index("=") as $i | {(.[:$i]): .[$i + 1:]}) | add | .sha = $sha' \
    --args "${fields[@]}" >> "$STATUSES"
fi
case $path in
  repos/sam/r/compare/*...*\?per_page=1)
    p=${path#repos/sam/r/compare/}
    f=$COMPARES/${p%\?per_page=1}.json
    [ -f "$f" ] || exit 1
    ;;
  */contents/*)
    p=${path#*/contents/}
    raw=${p%%\?ref=*}
    f=$TREES/${p##*\?ref=}/$(printf '%b' "$(sed 's/%/\\x/g' <<<"$raw")")
    [ -f "$f" ] || exit 1
    ;;
  repos/sam/r/pulls/7)
    f=$PR
    if [ -n "$RESTLESS" ]; then
      jq '.body += "!"' "$PR" > "$PR.next"
      mv "$PR.next" "$PR"
    fi
    ;;
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


def run(files, body="", trees=None, growth=growth_default, fail=False, fork=False, changes=None,
        restless=False, compares=None, pr_head="4ead"):
    """The job, for an event naming head 4ead, on a PR whose head is pr_head
    with these files (the comparison of base ba5e and 4ead, merge base 3e7b)
    and body. compares: the files of other comparisons, {(base, head):
    files}. changes: what happens before its Nth post, {N: {"body", "head"
    (its sha), "base" (sha, ref), "others" (statuses other runs post
    meanwhile, as (sha, state))}}, each of them optional."""
    pr = os.path.join(tmp, "pr.json")
    head_repo = None if fork is None else {"full_name": "fork/r" if fork else "sam/r"}
    doc = {"body": body, "head": {"sha": pr_head, "repo": head_repo}, "base": {"sha": "ba5e", "ref": "main"}}
    json.dump(doc, open(pr, "w"))
    compare_dir = os.path.join(tmp, "compares")
    shutil.rmtree(compare_dir, ignore_errors=True)
    os.mkdir(compare_dir)
    for (b, h), fs in {("ba5e", "4ead"): files, **(compares or {})}.items():
        json.dump({"merge_base_commit": {"sha": "3e7b"}, "files": fs}, open(os.path.join(compare_dir, f"{b}...{h}.json"), "w"))
    changes_dir = os.path.join(tmp, "changes")
    shutil.rmtree(changes_dir, ignore_errors=True)
    for n, c in sorted((changes or {}).items()):
        d = os.path.join(changes_dir, str(n))
        os.makedirs(d)
        doc = json.loads(json.dumps(doc))
        if "body" in c:
            doc["body"] = c["body"]
        if "head" in c:
            doc["head"]["sha"] = c["head"]
        if "base" in c:
            doc["base"] = {"sha": c["base"][0], "ref": c["base"][1]}
        json.dump(doc, open(os.path.join(d, "pr.json"), "w"))
        open(os.path.join(d, "statuses"), "w").write("".join(
            json.dumps({"state": state, "context": "rust-first (#7)", "sha": sha, "by": "another run"}) + "\n"
            for sha, state in c.get("others", ())))
    tree_dir = os.path.join(tmp, "trees")
    shutil.rmtree(tree_dir, ignore_errors=True)
    for sha, contents in (trees or {}).items():
        for path, text in contents.items():
            full = os.path.join(tree_dir, sha, path)
            os.makedirs(os.path.dirname(full), exist_ok=True)
            open(full, "w").write(text)
    log, statuses, posts = os.path.join(tmp, "gh.log"), os.path.join(tmp, "statuses"), os.path.join(tmp, "posts")
    for f in (log, statuses, posts):
        if os.path.exists(f):
            os.remove(f)
    runner_temp = os.path.join(tmp, "runner")
    shutil.rmtree(runner_temp, ignore_errors=True)
    os.mkdir(runner_temp)
    env = {"PATH": bin_dir + os.pathsep + os.environ["PATH"], "PR": pr, "COMPARES": compare_dir, "TREES": tree_dir, "GH_LOG": log, "STATUSES": statuses, "POSTS": posts, "CHANGES": changes_dir,
           "RESTLESS": "1" if restless else "", "GH_FAIL": "1" if fail else "",
           "GH_TOKEN": "t", "GITHUB_REPOSITORY": "sam/r", "GITHUB_SERVER_URL": "https://github.example",
           "GITHUB_RUN_ID": "99", "NUMBER": "7", "HEAD_SHA": "4ead", "GROWTH": str(growth), "RUNNER_TEMP": runner_temp}
    # GitHub runs `shell: bash` as this.
    p = subprocess.run([shutil.which("bash"), "--noprofile", "--norc", "-eo", "pipefail", script],
                       env=env, capture_output=True, text=True)
    calls = open(log).read().splitlines() if os.path.exists(log) else []
    posted = [json.loads(line) for line in open(statuses)] if os.path.exists(statuses) else []
    return p.returncode, p.stdout + p.stderr, calls, posted


def status(state, posted):
    """The one status a run posted: `state`, for this PR's head, linking the run."""
    assert len(posted) == 1, posted
    (s,) = posted
    assert s["state"] == state and s["context"] == "rust-first (#7)" and s["sha"] == "4ead", s
    assert s["target_url"] == "https://github.example/sam/r/actions/runs/99", s
    assert 0 < len(s["description"]) <= 140, s


def fails(files, body="", **kw):
    """It fails: the job passes, posting a failed status, or for a fork's PR
    fails, posting none. The error names what it wants."""
    rc, out, _, posted = run(files, body, **kw)
    errors = [line for line in out.splitlines() if line.startswith("::error")]
    assert len(errors) == 1 and "Why not Rust:" in errors[0], (files, body, rc, out)
    if kw.get("fork", False) is not False:
        assert rc == 1 and not posted, (files, body, rc, out, posted)
    else:
        assert rc == 0, (files, body, rc, out)
        status("failure", posted)
    return errors[0]


def passes(files, body="", **kw):
    rc, out, calls, posted = run(files, body, **kw)
    assert rc == 0 and "::error" not in out, (files, body, rc, out)
    if kw.get("fork", False) is not False:
        assert not posted, posted
    else:
        status("success", posted)
    return out, calls


REASON = "Why not Rust: a throwaway script for one migration."

# No Python in the change: it passes, body or none, and reads the PR as it is
# now, and its files as the comparison of its base and the event's head,
# which never change.
_, calls = passes([change("modified", "src/main.rs", 300, 10), change("added", "docs/x.md", 40)])
assert calls[0] == "api repos/sam/r/pulls/7", calls
assert calls[1] == "api repos/sam/r/compare/ba5e...4ead?per_page=1", calls
assert calls[2].startswith("api -X POST repos/sam/r/statuses/4ead "), calls
assert calls[3:] == ["api repos/sam/r/pulls/7"], calls
print("no Python: ok")

# The case it's for: a new Python file and no reason. It fails, and names
# neither the files nor anything from the body in the error.
error = fails([change("added", "tools/new_tool.py", 120)], "Adds a tool.\n\n::warning::from the body")
assert "new_tool" not in error and "from the body" not in error, error
_, out, _, posted = run([change("added", "tools/new_tool.py", 120)], "::warning::from the body")
assert "::warning::from the body" not in out and "from the body" not in posted[0]["description"], (out, posted)
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
fails([change("renamed", "tools/b.py", growth_default + 1, 0, previous="tools/a.py")])
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

# A file with no extension is Python by its shebang at the head, and was
# Python by its shebang at the merge base (3e7b), the old side of the PR's
# diff: never the base branch's tip (ba5e), which may have moved on. Others
# aren't, and aren't read.
PY, SH = "#!/usr/bin/env python3\nprint()\n", "#!/usr/bin/env bash\necho\n"
fails([change("added", "bin/wend", 80)], trees={"4ead": {"bin/wend": PY}})
passes([change("added", "bin/tool", 80)], trees={"4ead": {"bin/tool": SH}})
passes([change("modified", "bin/wend", 200, 0), change("removed", "bin/old", 0, 300)],
       trees={"4ead": {"bin/wend": "#!/usr/bin/python\n"}, "3e7b": {"bin/wend": PY, "bin/old": "#!/usr/bin/python3\n"},
              "ba5e": {"bin/wend": PY}})
_, calls = passes([change("added", "README.md", 80), change("added", "x/y.sh", 9)])
assert not any("/contents/" in c for c in calls), calls
# The comparison, and with it the merge base, is asked for once, however
# many paths need it.
_, calls = passes([change("modified", "bin/a", 3, 1), change("modified", "bin/b", 3, 1)],
                  trees={"4ead": {"bin/a": SH, "bin/b": SH}, "3e7b": {"bin/a": SH, "bin/b": SH}})
assert sum("/compare/" in c for c in calls) == 1, calls
# Paths are sent whole and escaped: a space, a percent sign, a backslash, a
# tab, a newline.
for odd in ("my dir/run 100%", "bin/we\\ird", "bin/tab\there", "bin/new\nline"):
    fails([change("added", odd, 5)], trees={"4ead": {odd: "#! /usr/bin/env python\n"}})
    passes([change("removed", odd, 0, 5)], trees={"3e7b": {odd: "#! /usr/bin/env python\n"}})
# And printed quoted: a name can't start a line of its own, where the runner
# would take it for a workflow command.
_, out, _, posted = run([change("added", "tools/a\n::warning::b.py", 5)])
assert not any(line.startswith("::warning") for line in out.splitlines()), out
status("failure", posted)
print("Python by its shebang: ok")

# A file that changes language, by its name or its shebang, counts as the
# language it becomes: into Python is new Python, whatever its lines; out of
# Python gives back the lines its diff takes out.
fails([change("modified", "bin/run", 10, 10)], trees={"4ead": {"bin/run": PY}, "3e7b": {"bin/run": SH}})
fails([change("changed", "bin/run", 1, 1)], trees={"4ead": {"bin/run": PY}, "3e7b": {"bin/run": SH}})
passes([change("modified", "m/a.py", 200, 0), change("modified", "bin/run", 30, 300)],
       trees={"4ead": {"bin/run": SH}, "3e7b": {"bin/run": PY}})
passes([change("modified", "m/a.py", 200, 0), change("renamed", "src/a.rs", 250, 300, previous="tools/a.py")])
print("files that change language: ok")

# GitHub lists 300 of a comparison's files at most: one it leaves out could
# be Python, so a PR with that many needs the line whatever they hold.
MANY = [change("modified", f"src/{i}.rs", 1, 1) for i in range(300)]
fails(MANY)
passes(MANY, REASON)
passes(MANY[1:])
print("a file list GitHub cut short: ok")

# GitHub not answering fails it, a file's content included: no answer is no
# verdict, never "not Python".
rc, out, _, posted = run([change("added", "tools/new_tool.py", 120)], REASON, fail=True)
assert rc != 0 and not posted, out
rc, out, _, posted = run([change("added", "bin/wend", 80)], REASON, trees={})
assert rc != 0 and "the body says" not in out and not posted, out
rc, out, _, posted = run([change("removed", "bin/wend", 0, 80)], REASON, trees={"ba5e": {"bin/wend": PY}})
assert rc != 0 and "the body says" not in out and not posted, out
print("no answer from GitHub: ok")

# Runs overlap: every push and every edit starts one, and the newest status
# wins. A run that read the PR before an edit can post after the edit's own
# run; reading the PR again after it posts, it posts the verdict for the PR
# as it is now, so the last word is the newest PR's, whichever way it goes.
NEW = [change("added", "tools/new_tool.py", 120)]
RUST = [change("modified", "src/main.rs", 1, 1)]


def said(posted):
    return [(p["sha"], p["state"], p.get("by", "this run")) for p in posted]


# The reason taken out after this run read the PR: the edit's run posts its
# failure first, then this one its old success, and then, having read the PR
# again, failure. Its files are read once: the head and base held.
rc, _, calls, posted = run(NEW, REASON, changes={1: {"body": "Adds a tool.", "others": [("4ead", "failure")]}})
assert rc == 0 and said(posted) == [("4ead", "failure", "another run"), ("4ead", "success", "this run"),
                                    ("4ead", "failure", "this run")], posted
assert sum("/compare/" in c for c in calls) == 1, calls
# The reason put in: the last word is success.
rc, _, _, posted = run(NEW, "Adds a tool.", changes={1: {"body": REASON, "others": [("4ead", "success")]}})
assert rc == 0 and said(posted)[1:] == [("4ead", "failure", "this run"), ("4ead", "success", "this run")], posted
# With no Python an edit changes nothing, but it's judged again all the same.
rc, _, _, posted = run(RUST, changes={1: {"body": "edited"}})
assert rc == 0 and said(posted) == [("4ead", "success", "this run")] * 2, posted
# A run posts only on the head its event named, where its own check keeps the
# head pending while it works: after a push, the new head is its own run's to
# judge, and this one stops. So does a run whose head the PR has already left
# (a re-run of an old one): it posts nothing.
rc, out, _, posted = run(RUST, changes={1: {"head": "5ead"}}, compares={("ba5e", "5ead"): NEW})
assert rc == 0 and said(posted) == [("4ead", "success", "this run")], posted
assert "no longer 4ead" in out, out
rc, _, calls, posted = run(NEW, pr_head="5ead", compares={("ba5e", "5ead"): RUST})
assert rc == 0 and not posted and not any("/compare/" in c for c in calls), (calls, posted)
# A new base: the diff it judges is the new one, the comparison of the new
# base and its head.
rc, _, calls, posted = run(NEW, changes={1: {"base": ("de5e", "dev")}}, compares={("de5e", "4ead"): RUST})
assert rc == 0 and said(posted) == [("4ead", "failure", "this run"), ("4ead", "success", "this run")], posted
assert [c for c in calls if "/compare/" in c] == ["api repos/sam/r/compare/ba5e...4ead?per_page=1",
                                                  "api repos/sam/r/compare/de5e...4ead?per_page=1"], calls
# A PR that changes every time it's read: after ten looks it fails, until
# it's edited again.
rc, out, _, posted = run(NEW, REASON, restless=True)
assert rc == 0 and said(posted) == [("4ead", "success", "this run")] * 10 + [("4ead", "failure", "this run")], posted
assert "kept changing" in posted[-1]["description"] and "::error" in out, out
print("runs that overlap, and a PR that changes while it's judged: ok")

# A fork's PR (or one whose fork is gone) runs with a token that can't post
# a status: the job's result is its verdict, and nothing is posted.
for fork in (True, None):
    fails([change("added", "tools/new_tool.py", 120)], fork=fork)
    passes([change("added", "tools/new_tool.py", 120)], REASON, fork=fork)
    passes([change("modified", "src/main.rs", 1, 1)], fork=fork)
print("a fork's PR: ok")
