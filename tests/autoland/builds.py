"""proofBuilds: a proof with nothing new to build skips its repo's line.
What it builds is worked out once a head, from the steward's mirror at that
exact commit; one whose outputs are being built by a proof already running
starts at once and holds no slot, and says so, and one whose outputs are
all in the store takes no job: its verdict is taken from the store, posted
while GitHub says it's waiting, saying so and naming the derivation. A slot is held by a build, not
a job: a job whose paths the jobs started before it are building holds
none, and one whose eval failed, or whose check has no attribute, holds its
own. A job on a head its PR no longer has keeps building only what the new
head's proof wants."""

import itertools
import json
import os
import re
import sys

import proofs as P
from harness import C, CI_GREEN, FAIL, JOBS, STATES, STOPPED, S, V, WROTE, clean, fresh_pr_state, merged, pr_state, run, sh, tmp
from prs import ci_state

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
    # A derivation is in the store until it's collected; an output once valid.
    print("\\n".join(p for p in a[2:] if ((p in know.get("collected", [])) if p.endswith(".drv") else (p not in know["valid"]))))
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


kept, kept_build = C.job, P.build
ids = itertools.count(1)
C.job = lambda repo, task, clone=False, since=None: JOBS.append(task) or f"job-{next(ids)}"
# A check with an attribute is built by the steward, not a Claude job: each
# build started is in JOBS too, as "build N CHECK...".
P.build = lambda repo, pr, checks, st: JOBS.append(f"build {pr['number']} {' '.join(checks)}") or f"{C.BUILD}{next(ids)}"
S.CFG["repos"]["sam/r"] = {"proofJobs": {"remi-proof": "Run `just proof` and post the remi-proof status."},
                           "proofBuilds": {"remi-proof": "checks.x86_64-linux.proof"}, "proofSlots": 1}
proof = CI_GREEN + [{"__typename": "StatusContext", "context": "remi-proof", "state": "PENDING", "description": ""}]
RUNNING, SKIPPED = "building remi-proof here", "nothing new to build, so it skipped the line"
RUNNING_JOB = "a Claude job is running remi-proof"  # a check with no attribute
CANT = "Settled, but what remi-proof builds can't be worked out here yet"
STORE = "Settled; remi-proof taken from the store, not run again"


GITHUB = {}  # each head's statuses by context, as a fresh read gets them
kept_gh, kept_write = C.gh, C.write


def read_status(*a, **k):
    m = re.fullmatch(r"repos/sam/r/commits/(\w+)/status\?per_page=100", a[1]) if a[:1] == ("api",) else None
    if not m:
        return kept_gh(*a, **k)
    if "status-read" in FAIL:
        return None
    return {"statuses": [{"context": c, "state": v} for c, v in GITHUB.get(m[1], {}).items()]}


def write_status(*a, **k):
    got = kept_write(*a, **k)
    m = re.fullmatch(r"repos/sam/r/statuses/(\w+)", a[1]) if a[:1] == ("api",) else None
    if m and got is not None:
        args = dict(x.split("=", 1) for x in a[2:] if "=" in x)
        if args["context"] != "review-settled":
            GITHUB.setdefault(m[1], {})[args["context"]] = args["state"]
    return got


C.gh, C.write = read_status, write_status


def from_store():
    """The remi-proof verdicts this pass posted: each one's description."""
    return [w.split("description=")[1].split(" -f ")[0] for w in WROTE
            if "/statuses/" in w and "context=remi-proof" in w and "state=success" in w]


def taken(drv):
    return f"Taken from the store, not run again: {drv}.drv passed here before"


def line(n, t=700, checks=None):
    return run(clean(checks=checks or proof, number=n, sha=heads[n]), t)[0]


def ahead(k, verb="build"):
    return f"waiting its turn to {verb} remi-proof ({k} ahead)"


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
# PR 3's Z is built already: no job, while PR 1 holds the slot; its verdict
# is taken from the store, saying so and naming Z's derivation.
status = line(3)
assert STORE in status and JOBS == [] and from_store() == [taken(Z)], (status, JOBS, WROTE)
# PR 4 builds X, which PR 1's job is building: Nix builds it once.
status = line(4)
assert RUNNING in status and SKIPPED in status and len(JOBS) == 1, (status, JOBS)
# The next pass: the two building X are one build, the one that skipped
# still says why, and PR 3's verdict, posted already, isn't again.
P.begin("sam/r", {1, 2, 3, 4})
assert ahead(1) in line(2, 800) and JOBS == [], JOBS
assert STORE in line(3, 800) and from_store() == [] and JOBS == [], WROTE
assert SKIPPED in line(4, 800) and JOBS == []
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

# A build whose eval fails has nothing to build: no build and no slot, a
# strike, and the eval tried again EVAL_RETRY minutes on. A check with no
# attribute is never worked out: a Claude job's, waiting its turn.
fresh_pr_state()
nix_knows()
P.begin("sam/r", {1, 5, 6})
line(1)
status = line(5)
assert CANT in status and JOBS == [] and pr_state(5)["strikes"] == {"eval": 1}, (status, JOBS)
assert CANT in line(5, 800) and len(evals()) == 2, evals()
assert CANT in line(5, 700 + P.EVAL_RETRY * 60) and len(evals()) == 3, evals()
S.CFG["repos"]["sam/r"]["proofBuilds"] = {}
VALID.append(W)  # PR 6's, had it been asked
nix_knows()
P.begin("sam/r", {1, 5, 6})
assert ahead(1, "run") in line(6) and evals() == [], evals()
# And a job whose paths are unknown holds a slot of its own: PR 1's build
# of X is one, PR 6's job the other.
S.CFG["repos"]["sam/r"]["proofSlots"] = 2
P.begin("sam/r", {1, 5, 6})
assert RUNNING_JOB in line(6, 900) and len(JOBS) == 1, JOBS
assert ahead(2, "run") in line(6, 1000) or RUNNING_JOB in line(6, 1000), JOBS

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
assert STORE in status and from_store() == [taken(Z)] and len(evals()) == 3, (status, evals())
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

# Taken from the store whatever the repo's proofSlots, and a verdict GitHub
# didn't take is posted again on a later pass, the PR left pending until it
# is: the store's word is the only one.
fresh_pr_state()
GITHUB.clear()
VALID[:] = [Z]
nix_knows()
del S.CFG["repos"]["sam/r"]["proofSlots"]
P.begin("sam/r", {3})
FAIL.append("context=remi-proof")
status = line(3)
assert STORE in status and JOBS == [] and heads[3] not in GITHUB, (status, JOBS)
assert pr_state(3)["strikes"] == {"status": 1}, pr_state(3)
FAIL.remove("context=remi-proof")
status = line(3, 800)
assert STORE in status and JOBS == [] and from_store() == [taken(Z)], (status, WROTE)
assert GITHUB[heads[3]] == {"remi-proof": "success"} and not pr_state(3)["strikes"], pr_state(3)
# The rollup a pass read before that status: nothing posted again.
assert STORE in line(3, 900) and from_store() == [] and JOBS == [], WROTE
# Set back to pending on the same head: posted again.
GITHUB[heads[3]]["remi-proof"] = "pending"
assert STORE in line(3, 1000) and from_store() == [taken(Z)], WROTE
# Remapped to another derivation while it's pending: that one's named.
VALID[:] = [Z, W]
nix_knows({3: Z, (3, "checks.x86_64-linux.other"): W})
S.CFG["repos"]["sam/r"]["proofBuilds"] = {"remi-proof": "checks.x86_64-linux.other"}
GITHUB[heads[3]]["remi-proof"] = "pending"
assert STORE in line(3, 1100) and from_store() == [taken(W)], WROTE
S.CFG["repos"]["sam/r"]["proofBuilds"] = {"remi-proof": "checks.x86_64-linux.proof"}
# Red by now, though this pass's rollup read it pending: left red.
GITHUB[heads[3]]["remi-proof"] = "failure"
assert STORE in line(3, 1200) and from_store() == [], WROTE
assert GITHUB[heads[3]]["remi-proof"] == "failure"
# GitHub can't say where it stands: nothing posted, a strike, tried again.
GITHUB[heads[3]]["remi-proof"] = "pending"
FAIL.append("status-read")
assert STORE in line(3, 1300) and from_store() == [] and pr_state(3)["strikes"] == {"status": 1}, WROTE
FAIL.remove("status-read")
assert STORE in line(3, 1400) and from_store() == [taken(Z)] and not pr_state(3)["strikes"], WROTE
# Each check's on its own: another check added on the same head, after the
# first's verdict, is posted alone, and so is whichever is still pending.
TWO = {"remi-proof": "checks.x86_64-linux.proof", "other-proof": "checks.x86_64-linux.other"}
pending_two = proof + [{"__typename": "StatusContext", "context": "other-proof", "state": "PENDING", "description": ""}]
pending_other = CI_GREEN + [{"__typename": "StatusContext", "context": "remi-proof", "state": "SUCCESS", "description": ""},
                            pending_two[-1]]


def other_store():
    return [w.split("description=")[1].split(" -f ")[0] for w in WROTE
            if "/statuses/" in w and "context=other-proof" in w and "state=success" in w]


VALID[:] = [Z, W]
nix_knows({3: Z, (3, "checks.x86_64-linux.other"): W})
S.CFG["repos"]["sam/r"]["proofJobs"]["other-proof"] = "Run the other proof."
S.CFG["repos"]["sam/r"]["proofBuilds"] = TWO
GITHUB[heads[3]] = {"remi-proof": "success"}
status = line(3, 1500, pending_two)
assert from_store() == [] and other_store() == [taken(W)] and JOBS == [], (status, WROTE)
# A rollup with only the second pending, the first already passed: the same.
GITHUB[heads[3]] = {"remi-proof": "success"}
status = line(3, 1600, pending_other)
assert from_store() == [] and other_store() == [taken(W)] and JOBS == [], (status, WROTE)
# One taken, the next refused: only the refused one is posted again.
GITHUB[heads[3]] = {}
FAIL.append("context=other-proof")
line(3, 1700, pending_two)
assert GITHUB[heads[3]] == {"remi-proof": "success"} and pr_state(3)["strikes"] == {"status": 1}, WROTE
FAIL.remove("context=other-proof")
line(3, 1800, pending_two)
assert from_store() == [] and other_store() == [taken(W)] and not pr_state(3)["strikes"], WROTE
# GitHub ignores a context's case, and its spelling can change between reads.
GITHUB[heads[3]] = {"REMI-PROOF": "failure", "Other-Proof": "error"}
line(3, 1900, pending_two)
assert from_store() == [] and other_store() == [], WROTE
GITHUB[heads[3]] = {"REMI-PROOF": "success", "Other-Proof": "pending"}
line(3, 2000, pending_two)
assert from_store() == [] and other_store() == [taken(W)], WROTE
GITHUB[heads[3]] = {"REMI-PROOF": "pending", "OTHER-PROOF": "pending"}
FAIL.append("context=other-proof")
line(3, 2100, pending_two)
assert from_store() == [taken(Z)] and pr_state(3)["strikes"] == {"status": 1}, WROTE
FAIL.remove("context=other-proof")
GITHUB[heads[3]] = {"Remi-Proof": "success", "OTHER-PROOF": "pending"}
line(3, 2200, pending_two)
assert from_store() == [] and other_store() == [taken(W)] and not pr_state(3)["strikes"], WROTE


# And so does the rollup's, which tells the same proof by the same rule: each
# check as GitHub spells it this pass, under the name the repo gives it.
def spelled(**states):
    return CI_GREEN + [{"__typename": "StatusContext", "context": c.replace("_", "-"), "state": v, "description": ""}
                       for c, v in states.items()]


def ci(**states):
    return ci_state(clean(checks=spelled(**states)), S.CFG["repos"]["sam/r"]["proofJobs"])[:2]


assert ci(REMI_PROOF="SUCCESS", Other_Proof="SUCCESS") == ("green", [])
assert ci(REMI_PROOF="FAILURE", Other_Proof="SUCCESS") == ("red", ["remi-proof"])
assert ci(REMI_PROOF="ERROR", OTHER_PROOF="FAILURE") == ("red", ["remi-proof", "other-proof"])
assert ci(REMI_PROOF="PENDING", other_proof="SUCCESS") == ("pending", ["remi-proof"])
assert ci(REMI_PROOF="SUCCESS") == ("pending", ["other-proof"])
assert ci(REMI_PROOF="PENDING", remi_proof="EXPECTED", other_proof="SUCCESS") == ("pending", ["remi-proof"])
assert ci(REMI_PROOF="SUCCESS", remi_proof="FAILURE", other_proof="SUCCESS") == ("red", ["remi-proof"])
assert ci(UNRELATED="PENDING", remi_proof="SUCCESS", other_proof="SUCCESS") == ("pending", ["UNRELATED"])
# A pending proof however spelled gets its verdict under the repo's name for
# it, and is one proof, not a second one to wait for.
fresh_pr_state()
GITHUB[heads[3]] = {"REMI-PROOF": "pending", "Other-Proof": "pending"}
status = line(3, 2300, spelled(REMI_PROOF="PENDING", Other_Proof="PENDING"))
assert from_store() == [taken(Z)] and other_store() == [taken(W)] and JOBS == [], (status, WROTE)
# Taken, then one refused: what's left goes on a later pass, spelled anew.
fresh_pr_state()
GITHUB[heads[3]] = {"REMI-PROOF": "pending", "other-proof": "pending"}
FAIL.append("context=other-proof")
line(3, 2400, spelled(REMI_PROOF="PENDING", other_proof="PENDING"))
assert from_store() == [taken(Z)] and pr_state(3)["strikes"] == {"status": 1}, WROTE
FAIL.remove("context=other-proof")
GITHUB[heads[3]] = {"Remi-Proof": "success", "OTHER-PROOF": "pending"}
line(3, 2500, spelled(Remi_Proof="SUCCESS", OTHER_PROOF="PENDING"))
assert from_store() == [] and other_store() == [taken(W)] and not pr_state(3)["strikes"], WROTE
# With what it builds not built, it's built, by the repo's name for it.
fresh_pr_state()
VALID[:] = []
nix_knows({3: Z, (3, "checks.x86_64-linux.other"): W})
status = line(3, 2600, spelled(REMI_PROOF="PENDING", other_proof="SUCCESS"))
assert RUNNING in status and JOBS == ["build 3 remi-proof"], (status, JOBS)
# Passed under another spelling: nothing left to prove, so it lands.
fresh_pr_state()
GITHUB[heads[3]] = {"REMI-PROOF": "success", "OTHER-PROOF": "success"}
status = line(3, 2700, spelled(REMI_PROOF="SUCCESS", OTHER_PROOF="SUCCESS"))
assert merged() and JOBS == [] and from_store() == [] and other_store() == [], (status, WROTE)
# Failed under another spelling: red, and the job that fixes it is told how
# the proof runs: a build's way, the steward's to post.
fresh_pr_state()
GITHUB[heads[3]] = {"REMI-PROOF": "failure", "other-proof": "success"}
red = spelled(REMI_PROOF="FAILURE", other_proof="SUCCESS")
line(3, 2800, red)
status = line(3, 10000, red)
assert not merged() and from_store() == [] and len(JOBS) == 1, (status, WROTE, JOBS)
assert "remi-proof isn't CI" in JOBS[0] and "`nix build .#checks.x86_64-linux.proof -L`" in JOBS[0], JOBS
assert "Run `just proof`" not in JOBS[0] and "never post that status yourself" in JOBS[0], JOBS
VALID[:] = [X]
del S.CFG["repos"]["sam/r"]["proofJobs"]["other-proof"]
S.CFG["repos"]["sam/r"]["proofBuilds"] = {"remi-proof": "checks.x86_64-linux.proof"}
# One output of two built isn't all of them: it's built.
VALID[:] = [X]
nix_knows({1: XY}, {XY: [X, Y]})
P.begin("sam/r", {1})
status = line(1)
assert RUNNING in status and from_store() == [] and len(JOBS) == 1, (status, WROTE)

del S.CFG["repos"]["sam/r"]
C.job, P.build = kept, kept_build
C.gh, C.write = kept_gh, kept_write
V.ORIGIN = kept_origin
P.OPEN.clear()
P.WAITING.clear()
STATES.clear()
fresh_pr_state()
print("proof builds: ok")
