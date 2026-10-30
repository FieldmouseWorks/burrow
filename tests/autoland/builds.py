"""proofBuilds: a proof with nothing new to build skips its repo's line.
What it builds is worked out once a head, from the steward's mirror at that
exact commit; one whose outputs are all in the store, or being built by a
proof already running, starts at once and holds no slot, and says so. A
slot is held by a build, not a job: a job whose paths the jobs started
before it are building holds none, and one whose eval failed, or whose
check has no attribute, holds its own. A job on a head its PR no longer
has keeps building only what the new head's proof wants."""

import itertools
import json
import os
import sys

import proofs as P
from harness import C, CI_GREEN, JOBS, STATES, STOPPED, S, V, clean, fresh_pr_state, pr_state, run, sh, tmp

# A repo whose PRs' heads are real commits on its remote, as GitHub has them.
origin, work = f"{tmp}/builds-origin", f"{tmp}/builds-work"
GIT = ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false"]
sh("git", "init", "-q", "--bare", f"{origin}/sam/r.git")
sh("git", "init", "-q", "-b", "main", work)
heads = {}
for n in range(1, 7):
    with open(f"{work}/f", "w") as f:
        f.write(f"{n}\n")
    sh(*GIT, "add", "f", cwd=work)
    sh(*GIT, "commit", "-qm", f"pr {n}", cwd=work)
    heads[n] = sh("git", "rev-parse", "HEAD", cwd=work).strip()
    sh("git", "push", "-q", f"{origin}/sam/r.git", f"HEAD:refs/pull/{n}/head", cwd=work)
kept_origin, V.ORIGIN = V.ORIGIN, f"{origin}/{{}}.git"

# nix and nix-store: each head's derivation as NIX says, its outputs as NIX
# says or that path less ".drv", and NIX's valid paths built. An eval reads its commit
# from the URL's repo, so a head the mirror lacks fails as it would.
NIX, NIX_LOG = f"{tmp}/nix.json", f"{tmp}/nix-log"
os.makedirs(f"{tmp}/nix")
for tool in ("nix", "nix-store"):
    with open(f"{tmp}/nix/{tool}", "w") as f:
        f.write(f"""#!{sys.executable}
import json, subprocess, sys, urllib.parse
a = sys.argv[1:]
with open({NIX_LOG!r}, "a") as f:
    f.write(json.dumps([{tool!r}, *a]) + "\\n")
know = json.load(open({NIX!r}))
if a[:2] == ["--check-validity", "--print-invalid"]:
    print("\\n".join(p for p in a[2:] if p not in know["valid"]))
elif a[:2] == ["--query", "--outputs"]:
    print("\\n".join(o for d in a[2:] for o in know.get("outs", {{}}).get(d, [d.removesuffix(".drv")])))
else:
    url, _, attr = a[-1].partition("#")
    u = urllib.parse.urlsplit(url)
    rev = urllib.parse.parse_qs(u.query)["rev"][0]
    git = subprocess.run(["git", "-C", urllib.parse.unquote(u.path), "cat-file", "-e", rev + "^{{commit}}"])
    drv = know["drvs"].get(rev + "#" + attr) or know["drvs"].get(rev)
    if git.returncode or not drv:
        print("error: no", rev, attr, file=sys.stderr)
        sys.exit(1)
    print(drv, end="")
""")
    os.chmod(f"{tmp}/nix/{tool}", 0o755)
X, Y, Z, W = (f"/nix/store/{c * 32}-proof-{c}" for c in "xyzw")
VALID = [Z]


def nix_knows(drvs=None, outs=None):
    """Each PR's derivation (by number, or by (number, attribute)), and a
    derivation's outputs where it has more than one."""
    drvs = drvs or {1: X, 2: Y, 3: Z, 4: X, 6: W}
    json.dump({"drvs": {heads[k] if isinstance(k, int) else f"{heads[k[0]]}#{k[1]}.drvPath": d + ".drv"
                        for k, d in drvs.items()},
               "outs": {d + ".drv": o for d, o in (outs or {}).items()}, "valid": VALID}, open(NIX, "w"))
    open(NIX_LOG, "w").close()


def evals():
    return [x for x in map(json.loads, open(NIX_LOG)) if x[0] == "nix"]


kept = C.job
ids = itertools.count(1)
C.job = lambda repo, task, clone=False: JOBS.append(task) or f"build-{next(ids)}"
S.CFG["repos"]["sam/r"] = {"proofJobs": {"remi-proof": "Run `just proof` and post the remi-proof status."},
                           "proofBuilds": {"remi-proof": "checks.x86_64-linux.proof"}, "proofSlots": 1}
proof = CI_GREEN + [{"__typename": "StatusContext", "context": "remi-proof", "state": "PENDING", "description": ""}]
RUNNING, SKIPPED = "a Claude job is running remi-proof", "nothing new to build, so it skipped the line"


def line(n, t=700):
    return run(clean(checks=proof, number=n, sha=heads[n]), t)[0]


def ahead(k):
    return f"waiting its turn to run remi-proof ({k} ahead)"


fresh_pr_state()
nix_knows()
P.begin("sam/r", {1, 2, 3, 4})
# PR 1 builds X, which isn't built: it takes the only slot.
status = line(1)
assert RUNNING in status and SKIPPED not in status and len(JOBS) == 1, (status, JOBS)
paths = pr_state(1)["proofPaths"]
assert list(paths) == [pr_state(1)["provedJob"]] and paths[pr_state(1)["provedJob"]]["paths"] == [X], paths
# PR 2 builds Y: it waits its turn.
assert ahead(1) in line(2) and JOBS == [], JOBS
# PR 3's Z is built already: it starts while PR 1 holds the slot, and says why.
status = line(3)
assert RUNNING in status and SKIPPED in status and len(JOBS) == 1, (status, JOBS)
# PR 4 builds X, which PR 1's job is building: Nix builds it once.
status = line(4)
assert RUNNING in status and SKIPPED in status and len(JOBS) == 1, (status, JOBS)
# The next pass: the two building X are one build, the one with nothing to
# build holds nothing, and the ones that skipped still say why.
P.begin("sam/r", {1, 2, 3, 4})
assert ahead(1) in line(2, 800) and JOBS == [], JOBS
assert SKIPPED in line(3, 800) and SKIPPED in line(4, 800) and JOBS == []
# PR 1's job ends without X built (it failed): PR 4's, still building X,
# holds the slot now.
STATES[pr_state(1)["provedJob"]] = "failed"
P.begin("sam/r", {1, 2, 3, 4})
assert ahead(1) in line(2, 900) and JOBS == [], JOBS
# X built, PR 4's job is only posting its verdict: it holds nothing, and
# PR 2's turn comes.
VALID.append(X)
nix_knows()
P.begin("sam/r", {1, 2, 3, 4})
status = line(2, 1000)
assert RUNNING in status and SKIPPED not in status and len(JOBS) == 1, (status, JOBS)
# Each head is worked out once, at its commit in the steward's mirror,
# with import-from-derivation off.
fresh_pr_state()
VALID[:] = [Z]
nix_knows()
P.begin("sam/r", {1, 2})
line(1)
line(2)
line(2, 800)
mirror = f"{tmp}/state/{V.GIT}/sam__r.git"
assert evals() == [["nix", "--extra-experimental-features", "nix-command flakes", "eval", "--raw",
                    "--option", "allow-import-from-derivation", "false", "--option", "accept-flake-config", "false",
                    "--no-update-lock-file",
                    f"git+file://{mirror}?ref=refs/pr/{n}&rev={heads[n]}#checks.x86_64-linux.proof.drvPath"]
                   for n in (1, 2)], evals()

# What can't be worked out waits its turn like before: an eval that fails
# (tried once a head), and a check with no attribute (never tried).
fresh_pr_state()
nix_knows()
P.begin("sam/r", {1, 5, 6})
line(1)
assert ahead(1) in line(5) and JOBS == [], JOBS
assert ahead(1) in line(5, 800) and len(evals()) == 2, evals()
S.CFG["repos"]["sam/r"]["proofBuilds"] = {}
VALID.append(W)  # PR 6's, had it been asked
nix_knows()
P.begin("sam/r", {1, 5, 6})
assert ahead(1) in line(6) and evals() == [], evals()
# And a job whose paths are unknown holds a slot of its own: PR 1's job
# building X is one, PR 6's the other.
S.CFG["repos"]["sam/r"]["proofSlots"] = 2
P.begin("sam/r", {1, 5, 6})
assert RUNNING in line(6, 900) and len(JOBS) == 1, JOBS
assert ahead(2) in line(5, 900) and JOBS == [], JOBS

# Jobs count in the order they started: one whose paths those before it are
# building holds nothing, a subset or a cover of several alike; one with a
# path of its own still holds one. PR 1 builds X and Y; PR 2 only X, so
# PR 3's Z has the second slot; then PR 4's Y and Z are both being built,
# and PR 6's W waits behind two builds.
S.CFG["repos"]["sam/r"].update(proofSlots=2, proofBuilds={"remi-proof": "checks.x86_64-linux.proof"})
fresh_pr_state()
VALID[:] = []
XY = "/nix/store/" + "v" * 32 + "-proof-xy"
nix_knows({1: XY, 2: X, 3: Z, 4: "/nix/store/" + "u" * 32 + "-proof-yz", 6: W},
          {XY: [X, Y], "/nix/store/" + "u" * 32 + "-proof-yz": [Y, Z]})
P.begin("sam/r", {1, 2, 3, 4, 6})
assert RUNNING in line(1) and RUNNING in line(2) and len(JOBS) == 1, JOBS
status = line(3)
assert RUNNING in status and SKIPPED not in status and len(JOBS) == 1, (status, JOBS)
status = line(4)
assert RUNNING in status and SKIPPED in status and len(JOBS) == 1, (status, JOBS)
assert ahead(2) in line(6) and JOBS == [], JOBS
# PR 1's job ends without them: PR 2's builds X now, and PR 4's Y.
STATES[pr_state(1)["provedJob"]] = "failed"
P.begin("sam/r", {1, 2, 3, 4, 6})
assert ahead(3) in line(6, 800) and JOBS == [], JOBS
S.CFG["repos"]["sam/r"]["proofSlots"] = 1

# A check pointed at another attribute while its PR waits is worked out
# again: what the old one built says nothing about the new.
fresh_pr_state()
VALID[:] = [Z]
nix_knows({1: X, 2: Y, (2, "checks.x86_64-linux.other"): Z})
P.begin("sam/r", {1, 2})
line(1)
assert ahead(1) in line(2) and len(evals()) == 2, evals()
S.CFG["repos"]["sam/r"]["proofBuilds"] = {"remi-proof": "checks.x86_64-linux.other"}
status = line(2, 800)
assert RUNNING in status and SKIPPED in status and len(evals()) == 3, (status, evals())
assert evals()[-1][-1].endswith("#checks.x86_64-linux.other.drvPath"), evals()
S.CFG["repos"]["sam/r"]["proofBuilds"] = {"remi-proof": "checks.x86_64-linux.proof"}

# A PR pushed again while its proof builds: the job is left to finish when
# the new head's proof wants what it's building (its check unchanged), and
# that proof starts beside it at once, Nix building it once; it's stopped
# when the new head builds something else, which then takes its slot. Each
# head is worked out once.
sh("git", "push", "-q", "-f", f"{origin}/sam/r.git", f"{heads[5]}:refs/pull/1/head", cwd=work)
for wants, stops in ((X, False), (W, True)):
    fresh_pr_state()
    STOPPED.clear()
    VALID[:] = [Z]
    nix_knows({1: X, 2: Y, 5: wants})
    P.begin("sam/r", {1, 2})
    line(1)
    job = pr_state(1)["provedJob"]
    assert ahead(1) in line(2) and JOBS == [], JOBS
    P.begin("sam/r", {1, 2})
    status = run(clean(checks=proof, number=1, sha=heads[5]), 800)[0]
    assert STOPPED == ([job] if stops else []) and RUNNING in status and len(JOBS) == 1, (wants, STOPPED, status, JOBS)
    assert (SKIPPED in status) != stops, (wants, status)
    assert ahead(1) in line(2, 800) and JOBS == [], JOBS
    assert len(evals()) == 3, evals()
sh("git", "push", "-q", "-f", f"{origin}/sam/r.git", f"{heads[1]}:refs/pull/1/head", cwd=work)
STOPPED.clear()

del S.CFG["repos"]["sam/r"]
C.job = kept
V.ORIGIN = kept_origin
P.OPEN.clear()
P.WAITING.clear()
STATES.clear()
fresh_pr_state()
print("proof builds: ok")
