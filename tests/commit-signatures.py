"""The shared CI's commits job, its own script run with gh stood in for: a PR
commit signed by a key GitHub can't match to its committer fails it, and an
unsigned or verified one doesn't.

  python3 -I tests/commit-signatures.py .github/workflows/flake-check.yml
  nix build .#checks.x86_64-linux.commit-signatures
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile

import yaml

job = yaml.safe_load(open(sys.argv[1]))["jobs"]["commits"]
# Only a PR has commits of its own to check, and reading them needs no more
# than the tree does.
assert job["if"] == "github.event_name == 'pull_request'", job["if"]
assert job["permissions"] == {"contents": "read"}, job["permissions"]
step = next(s for s in job["steps"] if s.get("id") == "signatures")
assert step["shell"] == "bash"
assert step["env"]["BASE"] == "${{ github.event.pull_request.base.sha }}"
assert step["env"]["HEAD"] == "${{ github.event.pull_request.head.sha }}"

tmp = tempfile.mkdtemp()
script = os.path.join(tmp, "signatures.sh")
open(script, "w").write(step["run"])

# gh as the job calls it: `gh api [--paginate] PATH --jq EXPR`, answered from
# a compare response, and every call logged.
bin_dir = os.path.join(tmp, "bin")
os.mkdir(bin_dir)
gh = os.path.join(bin_dir, "gh")
open(gh, "w").write(f"""#!{shutil.which("bash")}
echo "$*" >> "$GH_LOG"
[ -z "$GH_FAIL" ] || exit 1
while [ $# -gt 0 ]; do
  [ "$1" = --jq ] && {{ exec jq -r "$2" "$COMPARE"; }}
  shift
done
cat "$COMPARE"
""")
os.chmod(gh, 0o755)


def commit(sha, reason, name="Sam Owner", email="sam@example.org"):
    return {"sha": sha, "commit": {"committer": {"name": name, "email": email},
                                   "verification": {"verified": reason == "valid", "reason": reason}}}


def run(commits, fail=False):
    compare = os.path.join(tmp, "compare.json")
    json.dump({"merge_base_commit": {"sha": "f0rk"}, "commits": commits}, open(compare, "w"))
    log = os.path.join(tmp, "gh.log")
    if os.path.exists(log):
        os.remove(log)
    env = {"PATH": bin_dir + os.pathsep + os.environ["PATH"], "COMPARE": compare, "GH_LOG": log,
           "GH_FAIL": "1" if fail else "", "GH_TOKEN": "t", "GITHUB_REPOSITORY": "sam/r",
           "BASE": "ba5e", "HEAD": "4ead", "RUNNER_TEMP": tmp}
    # GitHub runs `shell: bash` as this.
    p = subprocess.run([shutil.which("bash"), "--noprofile", "--norc", "-eo", "pipefail", script],
                       env=env, capture_output=True, text=True)
    calls = open(log).read().splitlines() if os.path.exists(log) else []
    return p.returncode, p.stdout + p.stderr, calls


# Verified and unsigned commits pass: a commit GitHub made (a squash, a web
# edit) and one made on a machine that doesn't sign.
rc, out, calls = run([commit("a1", "valid"), commit("b2", "unsigned", "remi-renovate", "renovate@example.org")])
assert rc == 0 and "::error" not in out, out
# It asks for exactly the PR's commits: what HEAD has that its merge base
# with BASE doesn't, every page of them.
assert all("repos/sam/r/compare/ba5e...4ead" in c for c in calls) and any("--paginate" in c for c in calls), calls
print("verified and unsigned: ok")

# The case it's for: a session's own key on a commit named for a person, so
# GitHub can't find the key on that person's account. Only that commit is
# named, with its committer and GitHub's reason, and how to recommit it.
rc, out, _ = run([commit("a1", "valid"), commit("c3", "unknown_key"), commit("b2", "unsigned")])
assert rc == 1, (rc, out)
errors = [line for line in out.splitlines() if line.startswith("::error")]
assert len(errors) == 1 and "c3 is signed" in errors[0] and "Sam Owner <sam@example.org> (unknown_key)" in errors[0], out
assert "--reset-author' f0rk" in errors[0], errors[0]
print("a signature GitHub can't match to its committer: ok")

# Any other signature GitHub doesn't verify fails it too.
for reason in ("invalid", "no_user", "bad_email", "unverified_email", "expired_key", "not_signing_key", "malformed_signature"):
    rc, out, _ = run([commit("d4", reason)])
    assert rc == 1 and f"({reason})" in out, (reason, rc, out)
print("every other unverified signature: ok")

# GitHub not answering fails it: no answer is no verdict.
rc, out, _ = run([commit("a1", "valid")], fail=True)
assert rc != 0, out
print("no answer from GitHub: ok")
