"""renovate-sweep with stand-ins for gh, proj and the notifier: which Renovate
PRs go to a Claude job, and which majors the owner's notice lists.

    python3 -I tests/renovate-sweep.py modules/updates/renovate-sweep.sh
"""

import json
import os
import shlex
import subprocess
import sys
import tempfile

SWEEP = open(sys.argv[1]).read()
REPO = "sam/r"
tmp = tempfile.mkdtemp()
bin_ = os.path.join(tmp, "bin")
os.makedirs(bin_)
src = os.path.join(tmp, "src")
subprocess.run(["git", "init", "-q", os.path.join(src, "r")], check=True)
subprocess.run(["git", "-C", os.path.join(src, "r"), "remote", "add", "origin", f"https://github.com/{REPO}.git"], check=True)


def stub(name, body):
    path = os.path.join(bin_, name)
    with open(path, "w") as f:
        f.write(f"#!{sys.executable} -I\nimport json, os, sys\nSTUB = {tmp!r}\n{body}")
    os.chmod(path, 0o755)
    return path


stub("gh", """a = sys.argv[1:]
if a[:2] == ["api", "user"]:
    print("me")
elif a[:2] == ["pr", "list"]:
    print(open(os.path.join(STUB, "prs.json")).read())
elif a[:2] == ["pr", "view"]:
    print(json.dumps({"comments": json.load(open(os.path.join(STUB, "comments.json"))).get(a[2], [])}))
""")
stub("proj", """open(os.path.join(STUB, "proj.log"), "a").write(json.dumps(sys.argv[1:]) + "\\n")""")
notify = stub("notify", """open(os.path.join(STUB, "notify.log"), "a").write(json.dumps(sys.argv[1:]) + "\\n")""")


def pr(n, labels=("major",), conclusion="SUCCESS"):
    return {"number": n, "title": f"Update foo{n} to v2", "headRefName": f"renovate/foo{n}-2.x",
            "headRefOid": f"{n:040x}", "labels": [{"name": x} for x in labels],
            "createdAt": "2099-01-01T00:00:00Z",
            "statusCheckRollup": [{"name": "ci", "conclusion": conclusion}]}


def verdict(n, v):
    return {"author": {"login": "me"}, "body": f"Read it.\n<!-- major-verdict: {v} {n:040x} -->"}


def sweep(prs, comments=None, covered=None):
    """Run one sweep; return (proj calls, the majors notice as (verb, body))."""
    for f in ("proj.log", "notify.log"):
        open(os.path.join(tmp, f), "w").close()
    json.dump(prs, open(os.path.join(tmp, "prs.json"), "w"))
    json.dump({str(k): v for k, v in (comments or {}).items()}, open(os.path.join(tmp, "comments.json"), "w"))
    state = tempfile.mkdtemp(dir=tmp)
    cov = ""
    if covered is not None:
        cov = os.path.join(state, "repos.json")
        json.dump(covered, open(cov, "w"))
    env = {"DIR": state, "SRC": src, "OWNER": "Sam", "NOTIFY": notify, "SETTLED": "review-settled", "COVERED": cov}
    script = "set -euo pipefail\n" + "".join(f"{k}={shlex.quote(v)}\n" for k, v in env.items()) + SWEEP
    subprocess.run(["bash", "-c", script], check=True, env={**os.environ, "PATH": bin_ + ":" + os.environ["PATH"]})
    calls = [json.loads(x) for x in open(os.path.join(tmp, "proj.log"))]
    notices = [json.loads(x) for x in open(os.path.join(tmp, "notify.log"))]
    majors = [(n[0], n[-1]) for n in notices if n[1] == "majors"]
    assert len(majors) == 1, notices
    return calls, majors[0]


url = f"https://github.com/{REPO}/pull/"

# A major with no verdict goes to a job for one; the notice has nothing.
calls, (verb, _) = sweep([pr(1)])
assert len(calls) == 1 and "#1 " in calls[0][-1] and verb == "close", (calls, verb)

# A held one is the owner's, covered by the steward or not.
calls, (verb, body) = sweep([pr(2)], {2: [verdict(2, "held")]}, covered=[REPO])
assert calls == [] and verb == "open" and url + "2" in body and "held" in body, (calls, body)

# An ok one, green, merges on the steward's own in a repo it covers ...
calls, (verb, _) = sweep([pr(3)], {3: [verdict(3, "ok")]}, covered=[REPO])
assert calls == [] and verb == "close", verb
# ... and is listed for the owner in one it doesn't, or with no steward at all.
for covered in (["sam/other"], None):
    calls, (verb, body) = sweep([pr(3)], {3: [verdict(3, "ok")]}, covered=covered)
    assert calls == [] and verb == "open" and url + "3" in body and "doesn't cover" in body, (covered, body)
# Not while its CI is red: that's a job's to fix first.
calls, (verb, _) = sweep([pr(4, conclusion="FAILURE")], {4: [verdict(4, "ok")]})
assert len(calls) == 1 and verb == "close", (calls, verb)

# A security fix merges itself, a major one too: no job is asked for a
# verdict on it, and it's never listed as held.
calls, (verb, _) = sweep([pr(5, labels=("major", "security"))], {5: [verdict(5, "held")]})
assert calls == [] and verb == "close", (calls, verb)

print("renovate-sweep: ok")
