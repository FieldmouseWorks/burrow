"""Proof derivation evaluation runs in its own unit (burrow-proof eval-*),
never inline in the steward's pass: the pass starts an eval unit (bounded to
one at a time per repo), settles the PR pending with a wake deadline and no
eval strike, and harvests end.json once the unit finishes and kicks the repo.
Merged-with-base evals run only for the PR next in line, not for every open PR
on each new base tip. Cut, MERGED_MIN and pass-deadline clamping are gone."""

import itertools
import json
import os
import subprocess
import sys
from unittest.mock import patch

import proofs as P
import waits
from harness import (
    C,
    CI_GREEN,
    JOBS,
    KICKS,
    PROOF_RUN,
    REAL,
    S,
    STATES,
    STOPPED,
    UNSTOPPABLE,
    WROTE,
    clean,
    fresh_pr_state,
    info,
    pr_state,
    run,
    sh,
    stand_in,
    tmp,
    upkeep_config,
)

stand_in()

# Inline-eval pass hacks are deleted.
for old in ("Cut", "MERGED_MIN", "pass_left", "derivations"):
    assert not hasattr(P, old), f"proofs.{old} should be deleted"

# A repo whose PRs have real commits on its remote.
origin, work = f"{tmp}/evals-origin", f"{tmp}/evals-work"
GIT = ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]
sh("git", "init", "-q", "--bare", "-b", "main", f"{origin}/sam/r.git")
sh("git", "init", "-q", "-b", "main", work)
heads = {}
for n in range(1, 5):
    with open(f"{work}/f", "w") as f:
        f.write(f"{n}\n")
    sh(*GIT, "add", "f", cwd=work)
    sh(*GIT, "commit", "-qm", f"pr {n}", cwd=work)
    heads[n] = sh("git", "rev-parse", "HEAD", cwd=work).strip()
    sh(
        "git",
        "push",
        "-q",
        f"{origin}/sam/r.git",
        f"HEAD:refs/pull/{n}/head",
        *(["HEAD:refs/heads/main"] if n == 1 else []),
        cwd=work,
    )
# PR 7 branches from PR 1 changing g; main moves to PR 3 (clean merge with PR 7).
sh("git", "checkout", "-q", "-b", "seven", heads[1], cwd=work)
with open(f"{work}/g", "w") as f:
    f.write("pr 7\n")
sh(*GIT, "add", "g", cwd=work)
sh(*GIT, "commit", "-qm", "pr 7", cwd=work)
heads[7] = sh("git", "rev-parse", "HEAD", cwd=work).strip()
sh("git", "push", "-q", f"{origin}/sam/r.git", f"HEAD:refs/pull/7/head", cwd=work)
sh("git", "push", "-q", f"{origin}/sam/r.git", f"{heads[3]}:refs/heads/main", cwd=work)
up_tree = sh("git", "merge-tree", "--write-tree", heads[3], heads[7], cwd=work).split()[0]

kept_upkeep, C.CFG["upkeep"] = C.CFG["upkeep"], upkeep_config(f"{tmp}/upkeep-evals.json", git=origin)

# Fake nix & nix-store recording every call.
NIX_DIR = f"{tmp}/evals-nix"
NIX_DB, NIX_LOG = f"{tmp}/evals-nix.json", f"{tmp}/evals-nix.log"
os.makedirs(NIX_DIR)
for tool in ("nix", "nix-store"):
    with open(f"{NIX_DIR}/{tool}", "w") as f:
        f.write(
            f"""#!{sys.executable}
import json, subprocess, sys, urllib.parse
a = sys.argv[1:]
with open({NIX_LOG!r}, "a") as f:
    f.write(json.dumps([{tool!r}, *a]) + "\\n")
know = json.load(open({NIX_DB!r}))
if a[:2] == ["--check-validity", "--print-invalid"]:
    print("\\n".join(p for p in a[2:] if ((p in know.get("collected", [])) if p.endswith(".drv") else (p not in know["valid"]))))
elif a[:2] == ["--query", "--references"]:
    print("")
elif a[:2] == ["--query", "--outputs"]:
    print("\\n".join(o for d in a[2:] for o in know.get("outs", {{}}).get(d, [d.removesuffix(".drv")])))
else:
    url, _, attr = a[-1].partition("#")
    u = urllib.parse.urlsplit(url)
    rev = urllib.parse.parse_qs(u.query)["rev"][0]
    git = subprocess.run(["git", "-C", urllib.parse.unquote(u.path), "cat-file", "-e", rev + "^{{commit}}"])
    tree = subprocess.run(["git", "-C", urllib.parse.unquote(u.path), "rev-parse", rev + "^{{tree}}"],
                          capture_output=True, text=True).stdout.strip()
    drv = know["drvs"].get(rev + "#" + attr) or know["drvs"].get(rev) or know["drvs"].get("tree:" + tree)
    if git.returncode or not drv:
        print("error: no", rev, attr, file=sys.stderr)
        sys.exit(1)
    print(drv, end="")
"""
        )
    os.chmod(f"{NIX_DIR}/{tool}", 0o755)

X, Y, Z = (f"/nix/store/{c * 32}-eval-{c}" for c in "xyz")
VALID = [Z]


def nix_knows(drvs=None):
    drvs = drvs or {1: X, 2: Y, 3: Z, 7: Y, f"tree:{up_tree}": Z}
    json.dump(
        {
            "drvs": {
                (heads[k] if isinstance(k, int) else k): d + ".drv"
                for k, d in drvs.items()
            },
            "valid": VALID,
        },
        open(NIX_DB, "w"),
    )
    open(NIX_LOG, "w").close()


def nix_evals():
    return [x for x in map(json.loads, open(NIX_LOG)) if x[0] == "nix"]


# Fake systemd-run on PATH that records every started unit.
bin_dir = f"{tmp}/evals-bin"
SRUN_LOG, SRUN_RC = f"{tmp}/evals-srun.log", f"{tmp}/evals-srun.rc"
os.makedirs(bin_dir)
with open(f"{bin_dir}/systemd-run", "w") as f:
    f.write(
        f"""#!/bin/sh
printf '%s\\n' "$*" >> {SRUN_LOG}
exit $(cat {SRUN_RC} 2>/dev/null || echo 0)
"""
    )
os.chmod(f"{bin_dir}/systemd-run", 0o755)
kept_path, os.environ["PATH"] = os.environ["PATH"], f"{bin_dir}:{os.environ['PATH']}"
kept_nix, S.CFG["nix"] = S.CFG["nix"], NIX_DIR
kept_build = P.build
ids = itertools.count(1)
P.build = lambda repo, pr, checks, st: JOBS.append(f"build {pr['number']} {' '.join(checks)}") or f"{C.BUILD}{next(ids)}"
S.CFG["repos"]["sam/r"] = {
    "proofJobs": {"remi-proof": "Run proof."},
    "proofBuilds": {"remi-proof": "checks.x86_64-linux.proof"},
    "proofSlots": 1,
}
proof = CI_GREEN + [{"__typename": "StatusContext", "context": "remi-proof", "state": "PENDING", "description": ""}]


def line(n, t=700):
    return run(clean(checks=proof, number=n, sha=heads[n]), t)[0]


def run_eval_unit(jid, service_result="success", exit_code="exited", exit_status="0"):
    folder = f"{tmp}/state/builds/{jid}"
    p = subprocess.run([PROOF_RUN, "eval-run", folder], capture_output=True, text=True)
    if service_result == "success" and p.returncode != 0:
        service_result, exit_status = "exit-code", str(p.returncode)
    env = {
        **os.environ,
        "SERVICE_RESULT": service_result,
        "EXIT_CODE": exit_code,
        "EXIT_STATUS": exit_status,
    }
    e = subprocess.run([PROOF_RUN, "eval-end", folder], env=env, capture_output=True, text=True)
    assert e.returncode == 0, e.stderr
    STATES[jid] = json.load(open(f"{folder}/end.json"))["state"]


# 1. Pass 1: PR 1 needs eval -> NO in-process nix eval runs; starts unit via
# burrow-proof eval-start, settles pending with no eval strike and wake deadline.
# Bounded concurrency: PR 2 in the same repo waits without starting a 2nd unit.
fresh_pr_state()
nix_knows()
KICKS.clear()
open(SRUN_LOG, "w").close()
P.begin("sam/r", {1, 2})
WAKES, kept_wake = [], C.wake
C.wake = lambda repo, at: WAKES.append(at) or kept_wake(repo, at)
st1 = line(1, 700)
C.wake = kept_wake
assert nix_evals() == [], f"pass ran nix eval in-process: {nix_evals()}"
assert "working out what remi-proof builds" in st1, st1
assert pr_state(1).get("strikes", {}) == {}, pr_state(1)
jid1 = pr_state(1)["proofEval"]["job"]
assert pr_state(1)["proofEval"]["at"] + P.EVAL_TIMEOUT + 300 in WAKES, WAKES
assert "eval-run" in open(SRUN_LOG).read() and "eval-end" in open(SRUN_LOG).read(), open(SRUN_LOG).read()

st2 = line(2, 700)
assert nix_evals() == [], nix_evals()
assert "waiting to work out what remi-proof builds" in st2, st2
assert "proofEval" not in pr_state(2) and pr_state(2).get("strikes", {}) == {}, pr_state(2)

# C.DRY sees active live proofEval without starting anything or striking.
AUTOLAND = os.path.dirname(P.__file__)
mods = [
    m
    for m in list(sys.modules.values())
    if os.path.dirname(getattr(m, "__file__", None) or "") == AUTOLAND and isinstance(vars(m).get("DRY"), bool)
]
state_mods = [
    m
    for m in list(sys.modules.values())
    if os.path.dirname(getattr(m, "__file__", None) or "") == AUTOLAND and vars(m).get("STATE") == C.STATE
]
kept_state, C.STATE = C.STATE, f"{tmp}/state/dry"
for m in state_mods:
    m.STATE = C.STATE
os.makedirs(C.STATE, exist_ok=True)
waits.begin()
for m in mods:
    m.DRY = True
try:
    with patch.object(C, "write", wraps=REAL["write"]) as writes:
        line(1, 750)
        line(2, 750)
    dry_out = " ".join(" ".join(call.args) for call in writes.call_args_list)
finally:
    for m in mods:
        m.DRY = False
    C.STATE = kept_state
    for m in state_mods:
        m.STATE = kept_state
assert "working out what remi-proof builds" in dry_out, dry_out
assert "worked out on the steward's next pass" in dry_out, dry_out
assert nix_evals() == [], nix_evals()

# 2. The eval unit runs, writes end.json, and kicks the repo; Pass 2 harvests
# it, starts PR 1's build, and starts PR 2's eval unit.
run_eval_unit(jid1)
assert any(k.endswith("/sam/r") for k in KICKS), KICKS
assert len(nix_evals()) == 1, nix_evals()
P.begin("sam/r", {1, 2})
st1_after = line(1, 800)
assert "building remi-proof here" in st1_after and JOBS == ["build 1 remi-proof"], (st1_after, JOBS)
assert "proofEval" not in pr_state(1) and pr_state(1)["proofOutputs"]["paths"] == [X], pr_state(1)
st2_after = line(2, 800)
assert "working out what remi-proof builds" in st2_after and "proofEval" in pr_state(2), (st2_after, pr_state(2))

# 3. Lost eval unit (vanished with no end.json) records failure + eval strike,
# retried after EVAL_RETRY.
jid2 = pr_state(2)["proofEval"]["job"]
STATES[jid2] = "failed"
P.begin("sam/r", {1, 2})
st2_lost = line(2, 900)
assert "can't be worked out here yet" in st2_lost and pr_state(2)["strikes"] == {"eval": 1}, (st2_lost, pr_state(2))
assert "proofEval" not in pr_state(2) and pr_state(2)["proofOutputs"]["paths"] is None, pr_state(2)
# Past EVAL_RETRY, starts a fresh eval unit.
P.begin("sam/r", {1, 2})
st2_retry = line(2, 900 + P.EVAL_RETRY * 60)
assert "working out what remi-proof builds" in st2_retry and "proofEval" in pr_state(2), (st2_retry, pr_state(2))

# 4. Moved head supersedes a running eval unit (stopping it, or queueing in stopping).
# While the old eval unit is still running in stopping, the same PR MUST NOT start a
# second concurrent eval unit; once it stops, the new head's eval starts.
jid2_new = pr_state(2)["proofEval"]["job"]
STOPPED.clear()
UNSTOPPABLE.add(jid2_new)
P.begin("sam/r", {1, 2})
st2_unstop = run(clean(checks=proof, number=2, sha=heads[3]), 900 + P.EVAL_RETRY * 60 + 10)[0]
assert jid2_new in pr_state(2).get("stopping", []), pr_state(2)
assert "proofEval" not in pr_state(2), f"started concurrent eval while old one still running: {pr_state(2)}"
assert "waiting to work out what remi-proof builds" in st2_unstop, st2_unstop
# Even when PR 2 is no longer in OPEN (e.g. closed while its eval unit is still running),
# another PR in the same repo MUST still wait for that live eval unit to finish.
P.begin("sam/r", {4})
st4_closed_busy = line(4, 900 + P.EVAL_RETRY * 60 + 15)
assert "waiting to work out what remi-proof builds" in st4_closed_busy, st4_closed_busy
assert "proofEval" not in pr_state(4), f"ignored live eval from PR outside OPEN: {pr_state(4)}"
UNSTOPPABLE.clear()
P.begin("sam/r", {1, 2})
st2_after_stop = run(clean(checks=proof, number=2, sha=heads[3]), 900 + P.EVAL_RETRY * 60 + 20)[0]
assert jid2_new in STOPPED, STOPPED
assert "working out what remi-proof builds" in st2_after_stop and "proofEval" in pr_state(2), (st2_after_stop, pr_state(2))

# 4b. A live merged eval that refuses to stop on a head move also blocks starting a
# new eval unit (preventing concurrent merged-ref updates on the same PR).
fresh_pr_state()
nix_knows()
info["defaultBranchRef"]["target"] = {"oid": heads[3]}
P.begin("sam/r", {7})
line(7, 700)
run_eval_unit(pr_state(7)["proofEval"]["job"])
P.begin("sam/r", {7})
line(7, 710)
jid7_m_old = pr_state(7)["mergedEval"]["job"]
STOPPED.clear()
UNSTOPPABLE.add(jid7_m_old)
P.begin("sam/r", {7})
st7_moved = run(clean(checks=proof, number=7, sha=heads[4]), 720)[0]
assert jid7_m_old in pr_state(7).get("stopping", []), pr_state(7)
assert "proofEval" not in pr_state(7) and "mergedEval" not in pr_state(7), pr_state(7)
assert "waiting to work out what remi-proof builds" in st7_moved, st7_moved
UNSTOPPABLE.clear()
P.begin("sam/r", {7})
run(clean(checks=proof, number=7, sha=heads[4]), 730)
assert jid7_m_old in STOPPED, STOPPED
del info["defaultBranchRef"]["target"]

# 5. Merged eval runs ONLY for the PR next in line, not while waiting behind
# another PR's slot, and once harvested updates the branch when built in store.
fresh_pr_state()
nix_knows()
info["defaultBranchRef"]["target"] = {"oid": heads[3]}
P.begin("sam/r", {1, 7})
# Give PR 1 its outputs and let it take the only proof slot.
line(1, 700)
run_eval_unit(pr_state(1)["proofEval"]["job"])
P.begin("sam/r", {1, 7})
line(1, 750)  # starts merged eval for PR 1 (already up-to-date? no, PR 1 is behind PR 3)
if "mergedEval" in pr_state(1):
    run_eval_unit(pr_state(1)["mergedEval"]["job"])
    P.begin("sam/r", {1, 7})
    line(1, 760)
assert "provedJob" in pr_state(1), pr_state(1)
# Give PR 7 its outputs: since PR 1 holds the slot, PR 7 waits in line and does
# NOT start a merged eval while waiting behind PR 1.
line(7, 770)
run_eval_unit(pr_state(7)["proofEval"]["job"])
P.begin("sam/r", {1, 7})
st7_wait = line(7, 780)
assert "waiting its turn" in st7_wait and "mergedEval" not in pr_state(7), (st7_wait, pr_state(7))
# Once PR 1's slot frees, PR 7 is next in line: starts merged eval unit, then updates branch.
STATES[pr_state(1)["provedJob"]] = "done"
P.begin("sam/r", {7})
st7_m = line(7, 800)
assert "working out what remi-proof builds brought up to date with main" in st7_m, st7_m
jid7_m = pr_state(7)["mergedEval"]["job"]
run_eval_unit(jid7_m)
P.begin("sam/r", {7})
st7_upd = line(7, 820)
assert "brought up to date with main, which has remi-proof built here already" in st7_upd, st7_upd
del info["defaultBranchRef"]["target"]

# 6. Remaining synchronous nix-store queries clamp to the pass deadline via
# common.remote and propagate C.Unavailable instead of burning 60s past it.
with C.pass_deadline(0):
    try:
        P.unbuilt([X])
    except C.Unavailable:
        pass
    else:
        raise AssertionError("P.unbuilt ignored expired pass_deadline")
seen_timeouts = []
real_run = C.subprocess.run


def capture_timeout(cmd, *args, **kwargs):
    seen_timeouts.append(kwargs.get("timeout"))
    return real_run(cmd, *args, **kwargs)


with C.pass_deadline(5), patch.object(C.subprocess, "run", side_effect=capture_timeout):
    P.unbuilt([X])
assert seen_timeouts and seen_timeouts[0] <= 5, seen_timeouts

# Restore harness globals.
del S.CFG["repos"]["sam/r"]
os.environ["PATH"], S.CFG["nix"], P.build = kept_path, kept_nix, kept_build
C.CFG["upkeep"] = kept_upkeep
P.OPEN.clear()
P.WAITING.clear()
STATES.clear()
fresh_pr_state()
print("proof eval units: ok")
