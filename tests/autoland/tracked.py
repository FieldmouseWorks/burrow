"""A red check whose cause a fix job found outside the PR (the machine's: a
runner killed, as #1476's `train / negative` was, #1484), tracked by an
issue or PR it named in its answers (claims.py, answer.rs): the head gets
no second job while that's open, and shows as pending on it; once it
closes, the steward runs the red run again through the CI owner's rerun
door (burrow-upkeep's `tracked`, the real one, on the CI fixture), not a
job. A red after it closed is the head's own, and a job takes it. Before
this, each such answer was one that ended without fixing it, and two failed
the PR for the owner, three jobs a PR for one machine cause."""

import itertools
import json
import os
import sys

import harness
from harness import ISSUES, JOBS, RERUNS, RUN_INFO, STATES, T, C, clean, fresh_pr_state, head, pr_state, stand_in, tmp, upkeep_config

stand_in()
kept_job, ids = C.job, itertools.count(1)


def job(repo, task, clone=False, since=None, role=None):
    JOBS.append(task)
    return f"job-{next(ids)}"


C.job = job
GH_SAYS, PROJ_SAYS = f"{tmp}/tracked-gh.json", f"{tmp}/tracked-proj.json"
os.makedirs(f"{tmp}/tracked-bin")
with open(f"{tmp}/tracked-bin/proj", "w") as f:
    f.write(f"""#!{sys.executable}
import json, sys
print(json.load(open({PROJ_SAYS!r})).get(sys.argv[2], "running"))
""")
# GitHub as the answers' poster sees it: the PR at its head, no thread.
with open(f"{tmp}/tracked-bin/gh", "w") as f:
    f.write(f"""#!{sys.executable}
import json, sys
a = sys.argv[1:]
method, path = a[3], a[4]
body = json.loads(sys.stdin.read()) if "--input" in a else None
pr = json.load(open({GH_SAYS!r}))
if path == "graphql":
    got = {{"data": {{"repository": {{"pullRequest": pr}}}}}} if "reviewThreads" in body["query"] else {{"data": {{}}}}
elif method == "GET":
    got = []
else:
    got = {{}}
sys.stdout.write("HTTP/1.1 200 OK\\r\\n\\r\\n" + json.dumps(got))
""")
for x in ("proj", "gh"):
    os.chmod(f"{tmp}/tracked-bin/{x}", 0o755)
with open(GH_SAYS, "w") as f:
    json.dump({"state": "OPEN", "headRefOid": head, "headRefName": "feat", "body": "",
               "reviewThreads": {"pageInfo": {"hasNextPage": False}, "nodes": []}}, f)
kept_upkeep = C.CFG["upkeep"]
C.CFG["upkeep"] = upkeep_config(f"{tmp}/upkeep-tracked.json", proj=f"{tmp}/tracked-bin/proj")
kept_path, os.environ["PATH"] = os.environ["PATH"], f"{tmp}/tracked-bin{os.pathsep}{os.environ['PATH']}"


def run(pr, after):
    with open(PROJ_SAYS, "w") as f:
        json.dump(STATES, f)
    return harness.run(pr, after)


def leaves(jid, said):
    """Job jid ends its turn, its answers file saying `said`."""
    os.makedirs(f"{tmp}/jobs/{jid}", exist_ok=True)
    with open(f"{tmp}/jobs/{jid}/answers.json", "w") as f:
        json.dump(said, f)
    STATES[jid] = "done"


TRACKER, ISSUE = "sam/infra#7", "repos/sam/infra/issues/7"
MACHINE = {"pushed": None, "threads": [], "description": None, "tracker": TRACKER,
           "comment": f"train / negative's runner was killed (exit 143): the machine's, tracked in {TRACKER}."}
negative = {"__typename": "CheckRun", "databaseId": 70000001, "name": "train / negative", "status": "COMPLETED",
            "conclusion": "FAILURE", "checkSuite": {"id": "s70", "status": "COMPLETED", "app": {"slug": "github-actions"},
            "workflowRun": {"databaseId": 70, "file": {"path": ".github/workflows/train.yml"},
                            "workflow": {"id": "train", "name": "train"}}}}
red = clean(checks=[negative])
fresh_pr_state()
STATES.clear()
ISSUES[ISSUE] = {"state": "open", "closed_at": None}
run(red, 0)
run(red, 7200)
assert len(JOBS) == 1 and "tracker is the issue or PR that tracks a red check's cause outside this PR" in JOBS[0], JOBS
# Its job finds the cause the machine's, names what tracks it, and its turn
# ends: its answers go up, and the head keeps what they named.
leaves("job-1", MACHINE)
run(red, 7300)
assert pr_state()["redFixedTracker"] == {"tracker": TRACKER, "at": head}, pr_state()
run(red, 7400)
# No second job: pending on the tracker, held, on every pass while it's open.
for t in (7500, 9000, 20000):
    status, digest = run(red, t)
    assert JOBS == [] and RERUNS == [] and digest == [], (t, JOBS, RERUNS, digest)
    assert "state=pending" in status and f"tracked in {TRACKER}" in status, (t, status)
print("a cause outside the PR, open: ok")

# It closes: the red run, from before it closed, is asked to run again
# whole, and no job takes the head.
ISSUES[ISSUE] = {"state": "closed", "closed_at": "2026-10-24T08:00:00Z"}
RUN_INFO["70"] = {"run_attempt": 1, "status": "completed", "conclusion": "failure", "run_started_at": "2026-10-24T00:01:00Z"}
status, digest = run(red, 30000)
assert RERUNS == ["70"] and JOBS == [] and digest == [], (RERUNS, JOBS, digest)
assert "state=pending" in status and "asked to run again" in status, status
# A pass that still reads the old red, the rerun going: nothing more.
status, _ = run(red, 30100)
assert RERUNS == ["70"] and JOBS == [] and "running again" in status, (RERUNS, JOBS, status)
print("a cause outside the PR, closed: ok")

# Red again, run since it closed: the head's own, so a job takes it.
RUN_INFO["70"] = {"run_attempt": 2, "status": "completed", "conclusion": "failure", "run_started_at": "2026-10-24T08:30:00Z"}
status, _ = run(red, 31000)
assert RERUNS == ["70"] and len(JOBS) == 1 and "a Claude job has it" in status, (RERUNS, JOBS, status)
assert "redFixedTracker" not in pr_state(), pr_state()
print("red again after it closed: ok")

ISSUES.clear()
RUN_INFO.clear()
RERUNS.clear()
STATES.clear()
fresh_pr_state()
C.job = kept_job
os.environ["PATH"] = kept_path
C.CFG["upkeep"] = kept_upkeep
