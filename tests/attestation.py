"""The shared CI's attestation step, its own script run with curl and gh stood
in for: a private repo's job signs its gate's result with its own token,
whose audience names the digest of the statement it keeps beside the token
(FieldmouseWorks/Conary, docs/ci-attestation.md), a failed gate is attested
as failed, a public repo's is left to actions/attest, and a run with no OIDC
token attests nothing. With legs, no leg attests: the gate job does, with
the same steps, and passes only when every leg passed on its commit, each
sharing out one plan, its log the legs' own, each as its leg wrote it.

  python3 -I tests/attestation.py .github/workflows/flake-check.yml
  nix build .#checks.x86_64-linux.attestation
"""

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import urllib.parse

import yaml

jobs = yaml.safe_load(open(sys.argv[1]))["jobs"]
job = jobs["check"]
assert job["permissions"]["id-token"] == "write", job["permissions"]
steps = job["steps"]
at = next(i for i, s in enumerate(steps) if s.get("id") == "subject")
step = steps[at]
assert step["shell"] == "bash"
# Every repo's push and PR, a private one's included; with legs, the gate
# job's, not a leg's.
assert step["if"] == ("${{ !cancelled() && steps.gate.outcome != 'skipped' && inputs.legs <= 1"
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

# With legs: each leg's log goes up as an artifact named for it, with its
# digest, its commit and its plan's digest as the leg's output, one name per
# leg, which the job
# passes on. The gate job, once every leg has finished, passed or failed,
# checks out and merges as they did, judges them, and attests with the very
# same steps as the gate's job with one leg, but for whether it runs.
leg_at = next(i for i, s in enumerate(steps) if s.get("id") == "leg")
leg_step, leg_up = steps[leg_at], steps[leg_at + 1]
assert steps[leg_at - 1]["id"] == "gate" and leg_at < at, [s.get("id") for s in steps]
assert leg_step["if"] == "${{ !cancelled() && steps.gate.outcome != 'skipped' && inputs.legs > 1 }}", leg_step["if"]
assert leg_step["env"] == {"LEG": "${{ matrix.leg }}"}, leg_step["env"]
assert leg_up["uses"].startswith("actions/upload-artifact@"), leg_up
assert leg_up["if"] == "${{ !cancelled() && steps.leg.outputs.name }}", leg_up["if"]
assert leg_up["with"] == {"name": "${{ steps.leg.outputs.name }}", "path": "${{ runner.temp }}/flake.log"}, leg_up["with"]
assert job["outputs"] == {f"leg-{k}": f"${{{{ steps.leg.outputs.leg-{k} }}}}" for k in range(1, 9)}, job["outputs"]
gate = jobs["gate"]
assert gate["needs"] == ["lane", "check"], gate["needs"]
assert gate["if"] == ("${{ !cancelled() && inputs.legs > 1"
                      " && (needs.check.result == 'success' || needs.check.result == 'failure') }}"), gate["if"]
assert gate["permissions"] == job["permissions"], gate["permissions"]
g = gate["steps"]
assert g[:2] == steps[:2], g[:2]
assert g[2]["uses"].startswith("actions/download-artifact@"), g[2]
assert g[2]["with"] == {"pattern": "flake-log-leg-*", "path": "${{ runner.temp }}/legs"}, g[2]["with"]
verdict = g[3]
assert verdict["id"] == "gate" and verdict["shell"] == "bash" and "if" not in verdict, verdict
assert verdict["env"] == {"LEGS": "${{ inputs.legs }}", "RESULT": "${{ needs.check.result }}",
                          "OUTPUTS": "${{ toJSON(needs.check.outputs) }}"}, verdict["env"]
assert g[4]["id"] == "subject", g[4]
assert {**g[4], "if": step["if"]} == step, g[4]
assert g[4]["if"] == ("${{ !cancelled() && steps.gate.outcome != 'skipped'"
                      " && (github.event_name == 'push' || github.event_name == 'pull_request') }}"), g[4]["if"]
assert g[5:] == steps[at + 1:], g[5:]

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

# The legs, each its own log; then the gate job's verdict on them, as GitHub
# gives it the legs' combined outputs and their artifacts.
LEGS = 3


def legs(logs, result="success", tamper=None, wrong_commit=None, missing=None, rename=None, other_plan=None,
         no_plan=None):
    """Each leg's step, then the gate job's: its exit code, what it said and
    the log it leaves to attest."""
    runner_temp = tempfile.mkdtemp(dir=tmp)
    outputs = {}
    for k in range(1, LEGS + 1):
        leg_temp = tempfile.mkdtemp(dir=tmp)
        open(os.path.join(leg_temp, "flake.log"), "w").write(logs[k - 1])
        if k != no_plan:
            open(os.path.join(leg_temp, "plan.json"), "w").write('{"entries": []}' if k == other_plan else '{"entries": [1]}')
        out = os.path.join(leg_temp, "output")
        open(out, "w").close()
        p = subprocess.run(["bash", "-eo", "pipefail", "-c", leg_step["run"]], cwd=repo, capture_output=True, text=True,
                           env={**env, "RUNNER_TEMP": leg_temp, "GITHUB_OUTPUT": out, "LEG": str(k)})
        assert p.returncode == 0, p.stderr
        said = dict(line.split("=", 1) for line in open(out).read().splitlines())
        name = said["name"]
        assert re.fullmatch(f"flake-log-leg-{k}-[0-9a-f-]{{36}}", name), name
        assert set(said) == {"name", f"leg-{k}"}, said
        if k == missing:
            continue
        value = said[f"leg-{k}"]
        name_, sha, checked, plan = value.split(" ")
        assert name_ == name and checked == git("rev-parse", "HEAD^{commit}", cwd=repo), value
        assert plan == ("none" if k == no_plan else hashlib.sha256(
            open(os.path.join(leg_temp, "plan.json"), "rb").read()).hexdigest()), value
        if k == wrong_commit:
            value = f"{name} {sha} {'0' * 40} {plan}"
        if k == rename:
            value = value.replace(f"leg-{k}-", "leg-9-", 1)
        outputs[f"leg-{k}"] = value
        # download-artifact: each artifact in a directory of its own name.
        os.makedirs(os.path.join(runner_temp, "legs", name))
        log = open(os.path.join(leg_temp, "flake.log")).read() + ("tampered\n" if k == tamper else "")
        open(os.path.join(runner_temp, "legs", name, "flake.log"), "w").write(log)
    p = subprocess.run(["bash", "-eo", "pipefail", "-c", verdict["run"]], cwd=repo, capture_output=True, text=True,
                       env={**env, "RUNNER_TEMP": runner_temp, "LEGS": str(LEGS), "RESULT": result,
                            "OUTPUTS": json.dumps(outputs)})
    return p.returncode, p.stdout + p.stderr, open(os.path.join(runner_temp, "flake.log")).read()


LOGS = ["first leg\n", "second leg\n", "third leg\n"]
code, said, log = legs(LOGS)
assert code == 0, said
assert log == "".join(f"leg {k} of {LEGS}:\n{LOGS[k - 1]}" for k in range(1, LEGS + 1)), log
print("with legs, the gate job passes when every leg did, its log theirs: ok")
for kw, why in (({"result": "failure"}, "the legs: failure"),
                ({"missing": 2}, "leg 2 of 3 left no log"),
                ({"tamper": 3}, "leg 3 of 3: its log here isn't the one it wrote"),
                ({"rename": 1}, "leg 1 of 3: its log here isn't the one it wrote"),
                ({"wrong_commit": 2}, "leg 2 of 3 checked 0000000000000000000000000000000000000000, not"),
                ({"other_plan": 3}, "leg 3 of 3 took its share of another plan than leg 1"),
                ({"other_plan": 1}, "leg 2 of 3 took its share of another plan than leg 1"),
                ({"no_plan": 2}, "leg 2 of 3 stopped before it read the plan")):
    code, said, log = legs(LOGS, **kw)
    assert code != 0 and why in said, (kw, code, said)
    # A leg whose log is its own keeps it in the gate's, however the leg went.
    if "other_plan" in kw or "no_plan" in kw:
        assert log == "".join(f"leg {k} of {LEGS}:\n{LOGS[k - 1]}" for k in range(1, LEGS + 1)), (kw, log)
print("a failed leg, a leg's log missing, changed or for another commit, or legs that split different plans, "
      "fail the gate job: ok")

