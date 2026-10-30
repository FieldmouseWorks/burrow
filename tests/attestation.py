"""The shared CI's attestation step, its own script run with curl and gh stood
in for: a private repo's job signs its gate's result with its own token,
whose audience names the digest of the statement it keeps beside the token
(FieldmouseWorks/Conary, docs/ci-attestation.md), a failed gate is attested
as failed, a public repo's is left to actions/attest, and a run with no OIDC
token attests nothing.

  python3 -I tests/attestation.py .github/workflows/flake-check.yml
  nix build .#checks.x86_64-linux.attestation
"""

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import urllib.parse

import yaml

job = yaml.safe_load(open(sys.argv[1]))["jobs"]["check"]
assert job["permissions"]["id-token"] == "write", job["permissions"]
steps = job["steps"]
at = next(i for i, s in enumerate(steps) if s.get("id") == "subject")
step = steps[at]
assert step["shell"] == "bash"
# Every repo's push and PR, a private one's included.
assert step["if"] == ("${{ !cancelled() && steps.gate.outcome != 'skipped'"
                      " && (github.event_name == 'push' || github.event_name == 'pull_request') }}"), step["if"]
assert step["env"]["PRIVATE"] == "${{ github.event.repository.private }}", step["env"]
assert step["env"]["OUTCOME"] == "${{ steps.gate.outcome }}", step["env"]
# After it: the log, the token's attestation (only when there is one), and
# the Sigstore bundle, for a public repo only.
log_up, token_up, bundle = steps[at + 1:at + 4]
assert log_up["uses"].startswith("actions/upload-artifact@"), log_up
assert log_up["with"]["name"] == "${{ steps.subject.outputs.log }}", log_up
assert token_up["uses"].startswith("actions/upload-artifact@"), token_up
assert token_up["if"] == "${{ !cancelled() && steps.subject.outputs.attestation }}", token_up["if"]
assert token_up["with"] == {"name": "${{ steps.subject.outputs.attestation-name }}",
                            "path": "${{ steps.subject.outputs.attestation }}"}, token_up["with"]
assert bundle["uses"].startswith("actions/attest@"), bundle
assert bundle["if"] == ("${{ !cancelled() && steps.subject.outputs.path"
                        " && !github.event.repository.private }}"), bundle["if"]

tmp = tempfile.mkdtemp()
script = os.path.join(tmp, "subject.sh")
open(script, "w").write(step["run"])

# curl as the step calls it: `curl -fsS -H HEADER URL`, every call logged,
# answering with a token; gh answers the compare a PR's subject asks for.
bin_dir = os.path.join(tmp, "bin")
os.mkdir(bin_dir)
TOKEN = "eyJhbGciOiJSUzI1NiJ9.eyJhdWQiOiJ4In0.c2ln"
curl = os.path.join(bin_dir, "curl")
open(curl, "w").write(f"""#!{shutil.which("bash")}
printf '%s\\n' "$@" >> "$CURL_LOG"
[ -z "$CURL_FAIL" ] || exit 22
echo '{{"count": 1, "value": "{TOKEN}"}}'
""")
os.chmod(curl, 0o755)
gh = os.path.join(bin_dir, "gh")
open(gh, "w").write(f"""#!{shutil.which("bash")}
echo "$MERGE_BASE"
""")
os.chmod(gh, 0o755)

env = {"PATH": os.environ["PATH"], "HOME": tmp, "GIT_CONFIG_NOSYSTEM": "1"}
GIT = ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"]


def git(*a, cwd):
    return subprocess.run([*GIT, *a], cwd=cwd, env=env, check=True, capture_output=True, text=True).stdout.strip()


def commit(cwd, f, text):
    open(os.path.join(cwd, f), "w").write(text)
    git("add", f, cwd=cwd)
    git("commit", "-q", "-m", f, cwd=cwd)
    return git("rev-parse", "HEAD", cwd=cwd)


repo = os.path.join(tmp, "repo")
os.mkdir(repo)
git("init", "-q", cwd=repo)
tip = commit(repo, "a", "a")


def run(private, outcome="success", event="push", oidc=True, fail=False, merge_base=""):
    """The step's outputs, the file it attested to and curl's arguments."""
    runner_temp = tempfile.mkdtemp(dir=tmp)
    open(os.path.join(runner_temp, "flake.log"), "w").write("checks passed\n")
    out = os.path.join(runner_temp, "output")
    curl_log = os.path.join(runner_temp, "curl.log")
    open(out, "w").close()
    open(curl_log, "w").close()
    e = {**env, "PATH": bin_dir + os.pathsep + env["PATH"], "RUNNER_TEMP": runner_temp,
         "GITHUB_OUTPUT": out, "GITHUB_EVENT_NAME": event, "GITHUB_SHA": git("rev-parse", "HEAD", cwd=repo),
         "GITHUB_REPOSITORY": "owner/repo", "OUTCOME": outcome, "SKIP": "agents",
         "PRIVATE": "true" if private else "false", "PR_HEAD": "", "CURL_LOG": curl_log,
         "CURL_FAIL": "1" if fail else "", "MERGE_BASE": merge_base, "GH_TOKEN": "t"}
    if oidc:
        e["ACTIONS_ID_TOKEN_REQUEST_URL"] = "https://token.example/idtoken?api-version=2.0"
        e["ACTIONS_ID_TOKEN_REQUEST_TOKEN"] = "request-token"
    if event == "pull_request":
        e["PR_HEAD"] = git("rev-parse", "HEAD^2", cwd=repo)
    p = subprocess.run(["bash", "-eo", "pipefail", script], cwd=repo, env=e, capture_output=True, text=True)
    outputs = dict(line.split("=", 1) for line in open(out).read().splitlines())
    attested = open(outputs["attestation"]).read() if "attestation" in outputs else None
    return p.returncode, outputs, attested, open(curl_log).read().splitlines()


def statement_of(attested):
    """The statement and the token, as a verifier splits the file."""
    statement, token = attested.rstrip("\n").rsplit("\n", 1)
    return statement + "\n", token


# A private repo's push: the subject, the predicate on one line, the token.
code, outputs, attested, args = run(private=True)
assert code == 0, code
statement, token = statement_of(attested)
assert token == TOKEN, token
lines = statement.splitlines()
assert lines[:2] == ["conary commit v1", git("rev-parse", "HEAD", cwd=repo)], lines
assert len(lines) == 3, lines
predicate = json.loads(lines[2])
assert predicate == {"result": "pass", "revision": None, "tree": git("rev-parse", "HEAD^{tree}", cwd=repo),
                     "skipped": ["agents"], "log": predicate["log"]}, predicate
assert open(outputs["path"]).read() + lines[2] + "\n" == statement, (outputs["path"], statement)
# Its audience names the statement's digest, and the request carries the
# run's own request token.
audience = "conary-attestation:v1:sha256:" + hashlib.sha256(statement.encode()).hexdigest()
assert "Authorization: bearer request-token" in args, args
assert args[-1] == "https://token.example/idtoken?api-version=2.0&audience=" + audience, args
url = urllib.parse.urlsplit(args[-1])
assert url.netloc == "token.example", url
query = urllib.parse.parse_qs(url.query)
assert query == {"api-version": ["2.0"], "audience": [audience]}, query
# The attestation's artifact shares the log's uuid.
uuid = outputs["log"].removeprefix("flake-log-")
assert outputs["attestation-name"] == "conary-attestation-" + uuid, outputs
assert predicate["log"]["artifact"] == outputs["log"], predicate
print("a private repo's push attests with its own token: ok")

# A failed gate is attested too, as failed.
code, outputs, attested, _ = run(private=True, outcome="failure")
assert code == 0, code
assert json.loads(statement_of(attested)[0].splitlines()[2])["result"] == "fail", attested
print("a failed gate attests a fail: ok")

# A public repo's: the subject for actions/attest, no token asked for.
code, outputs, attested, args = run(private=False)
assert code == 0 and attested is None and args == [], (code, attested, args)
assert set(outputs) == {"path", "log"}, outputs
print("a public repo's is left to the Sigstore bundle: ok")

# No OIDC token: nothing attested, and the job goes on.
code, outputs, attested, args = run(private=True, oidc=False)
assert code == 0 and outputs == {} and args == [], (code, outputs, args)
print("a run with no OIDC token attests nothing: ok")

# A token GitHub won't give fails the step rather than attest without one.
code, outputs, attested, _ = run(private=True, fail=True)
assert code != 0 and "attestation" not in outputs, (code, outputs)
print("a token request that fails fails the step: ok")

# A private repo's PR: the revision, the merge step's merge of the head onto
# the base branch's tip.
git("checkout", "-q", "-b", "pr", cwd=repo)
head = commit(repo, "b", "b")
git("checkout", "-q", "main", cwd=repo)
commit(repo, "c", "c")
git("merge", "-q", "--no-ff", "--no-edit", head, cwd=repo)
code, outputs, attested, _ = run(private=True, event="pull_request", merge_base=tip)
assert code == 0, code
statement, token = statement_of(attested)
lines = statement.splitlines()
assert lines[:3] == ["conary revision v1", tip, head], lines
assert json.loads(lines[3])["revision"] == {"base": tip, "head": head}, lines[3]
print("a private repo's PR attests its revision: ok")
