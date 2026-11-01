"""A proof check that's a command (proofRuns) is the steward's own to run,
with no Claude job: start_run writes what the run is, in the repo's real
clone in projectsDir, and has burrow-proof start its unit (the real one,
which reads what it's given, with systemd-run stood in for: the run itself
is crates/burrow-proof's, and its tests'). The unit's end leaves end.json
and comment.md, as burrow-proof's `end` writes them (its tests/ends.rs
holds it to that), and a pass posts them: the verdict, and the comment as
it is. Runs come after builds and before Claude jobs; a check's kind and
command are what its verdict belongs to."""

import json
import os
import time

import proofs as P
from harness import CI_GREEN, JOBS, PROOF_RUN, STATES, WROTE, C, S, clean, fresh_pr_state, pr_state, run, sh, stand_in, tmp

stand_in()

# The repo's clone in projectsDir, in a folder that isn't named for it.
projects = f"{tmp}/runs-projects"
checkout = f"{projects}/elsewhere"
sh("git", "init", "-q", checkout)
sh("git", "-C", checkout, "remote", "add", "origin", "https://github.com/sam/r")
kept_projects = S.CFG["projectsDir"]
S.CFG["projectsDir"] = projects
RUNS = S.CFG["runsDir"]
os.makedirs(RUNS)

# systemd-run, stood in for: it keeps how it was run, and exits as told.
bin_dir, ARGS, RC = f"{tmp}/runs-bin", f"{tmp}/runs-systemd-run.json", f"{tmp}/runs-systemd-run.rc"
os.makedirs(bin_dir)
with open(f"{bin_dir}/systemd-run", "w") as f:
    f.write(f"""#!/bin/sh
printf '%s\\n' "$@" >{ARGS}
exit $(cat {RC} 2>/dev/null || echo 0)
""")
os.chmod(f"{bin_dir}/systemd-run", 0o755)
kept_path = os.environ["PATH"]
os.environ["PATH"] = f"{bin_dir}:{kept_path}"


def systemd_run(rc=0):
    with open(RC, "w") as f:
        f.write(str(rc))
    if os.path.exists(ARGS):
        os.remove(ARGS)


def asked():
    return open(ARGS).read().splitlines() if os.path.exists(ARGS) else None


def folder(jid):
    return f"{tmp}/state/builds/{jid}"


fresh_pr_state()
systemd_run()
S.CFG["repos"]["sam/r"] = {"proofRuns": {"gate": "make gate", "other": "make other"}}
pr = clean()
jid = P.start_run("sam/r", pr, ["gate"], {})
assert jid and jid.startswith(C.BUILD + "sam-r-5-" + "a" * 12), jid
# What it runs, where: the clone's real place, whatever its folder's
# called; nothing of the PR's base.
assert json.load(open(f"{folder(jid)}/build.json")) == {
    "repo": "sam/r", "number": 5, "head": "a" * 40, "checkout": checkout, "runs": {"gate": "make gate"},
    "unit": C.build_unit(jid), "timeout": 180 * 60, "bash": S.CFG["bash"], "git": S.CFG["git"], "trees": RUNS,
    "kick": f"{S.CFG['kick']}/sam/r", "commentBytes": P.COMMENT_BYTES}, json.load(open(f"{folder(jid)}/build.json"))
# burrow-proof took it and handed systemd its unit, which it runs.
a = asked()
assert a and f"--unit={C.build_unit(jid)}" in a and a[-3:] == [os.path.realpath(PROOF_RUN), "run", folder(jid)], a
# systemd's refusal: none started.
systemd_run(1)
assert P.start_run("sam/r", pr, ["gate"], {}) is None
# A record burrow-proof won't take (a checkout it can't find from anywhere)
# never reaches systemd.
systemd_run()
S.CFG["projectsDir"] = "runs-projects"
kept_cwd = os.getcwd()
os.chdir(tmp)
assert P.start_run("sam/r", pr, ["gate"], {}) is None and asked() is None
os.chdir(kept_cwd)
S.CFG["projectsDir"] = projects
# No clone here at all: nothing to run it in.
S.CFG["projectsDir"] = f"{tmp}/none"
assert P.start_run("sam/r", pr, ["gate"], {}) is None and asked() is None
S.CFG["projectsDir"] = projects
print("proof runs' start: ok")

# A pass over a PR whose gate is a run: no Claude job, a run; its verdict
# posted once it's done, and its comment as burrow-proof wrote it.
HEAD = "c" * 40
GATE = CI_GREEN + [{"__typename": "StatusContext", "context": "gate", "state": "PENDING", "description": ""}]
STARTED = []
kept_start, kept_build, kept_gh = P.start_run, P.build, C.gh


def start_run(repo, pr, checks, st):
    """A run's folder, as start_run leaves it, with no unit."""
    jid = P.unit_id(repo, pr["number"], pr["headRefOid"])
    os.makedirs(folder(jid))
    C.save(f"builds/{jid}/build.json", {"repo": repo, "number": pr["number"], "head": pr["headRefOid"],
                                        "runs": {c: C.repo_cfg(repo)["proofRuns"][c] for c in checks}})
    STARTED.append(jid)
    return jid


def finish(jid, state="done", verdicts=None, comment=None):
    """Its unit's end, as burrow-proof's `end` leaves it."""
    end = {"state": state, "exit": 0 if state == "done" else None, "at": time.time()}
    if verdicts is not None:
        end |= {"verdicts": verdicts, "exits": {c: 0 if v[0] == "success" else 1 for c, v in verdicts.items()}}
    C.save(f"builds/{jid}/end.json", end)
    if comment is not None:
        with open(f"{folder(jid)}/comment.md", "w") as f:
            f.write(comment)
    STATES[jid] = state


def gh(*a, **k):
    """GitHub's word on the head's statuses: none settled yet."""
    if a[:1] == ("api",) and a[1].endswith("/status?per_page=100"):
        return {"statuses": []}
    return kept_gh(*a, **k)


def statuses(context="gate"):
    return [dict(x.split("=", 1) for x in w.split(" -f ")[1:]) for w in WROTE if "/statuses/" in w and f"context={context}" in w]


def comments():
    return [w for w in WROTE if "/comments" in w]


P.start_run, C.gh = start_run, gh
PASSED, FAILED = ["success", "Ran here and passed"], ["failure", "Ran here and failed: exit 1"]
for verdict, comment in ((PASSED, None), (PASSED, "`gate` passed on it.\n\n## 6 of 6\n"), (FAILED, "`gate` failed on it.\n")):
    fresh_pr_state()
    STARTED.clear()
    STATES.clear()
    S.CFG["repos"]["sam/r"] = {"proofRuns": {"gate": "make gate"}}
    status, _ = run(clean(sha=HEAD, checks=GATE), 700)
    assert JOBS == [] and len(STARTED) == 1 and "Settled; running gate here" in status, (status, JOBS, STARTED)
    assert pr_state()["provedFor"] == ["run", ["gate"], {"gate": "make gate"}], pr_state()
    # Still running: it says so, and nothing more starts.
    status, _ = run(clean(sha=HEAD, checks=GATE), 800)
    assert len(STARTED) == 1 and "Settled; running gate here" in status, status
    finish(STARTED[0], verdicts={"gate": verdict}, comment=comment)
    status, _ = run(clean(sha=HEAD, checks=GATE), 900)
    assert statuses() == [{"state": verdict[0], "context": "gate", "description": verdict[1], "target_url": "u"}], WROTE
    assert "Settled; gate ran here, its verdict posted" in status, status
    said = comments()
    if comment is None:
        assert said == [], said
    else:
        # As it is, before the status.
        assert len(said) == 1 and comment in said[0], (comment, said)
        assert WROTE.index(said[0]) < WROTE.index(next(w for w in WROTE if "/statuses/" in w)), WROTE
    run(clean(sha=HEAD, checks=GATE), 1000)
    assert comments() == [], WROTE
# One that failed (stopped, timed out, ended short): no verdict, and
# another runs it later.
for state in ("failed", "timed out"):
    fresh_pr_state()
    STARTED.clear()
    S.CFG["repos"]["sam/r"] = {"proofRuns": {"gate": "make gate"}}
    run(clean(sha=HEAD, checks=GATE), 700)
    finish(STARTED[0], state)
    run(clean(sha=HEAD, checks=GATE), 900)
    status, _ = run(clean(sha=HEAD, checks=GATE), 960)
    assert statuses() == [] and "Settled; the last run of gate" in status and "another starts at" in status, (status, WROTE)
    status, _ = run(clean(sha=HEAD, checks=GATE), 1100)
    assert len(STARTED) == 2 and "Settled; running gate here again" in status, (status, STARTED)
    assert statuses() == [] and JOBS == [], WROTE
# One that can't start says so.
fresh_pr_state()
P.start_run = lambda repo, pr, checks, st: None
status, _ = run(clean(sha=HEAD, checks=GATE), 700)
assert "Settled, but the run of gate couldn't start" in status and JOBS == [], status
P.start_run = start_run
# Builds go first, then runs, then Claude jobs; each starts afresh, not as
# the last one's second go.
BUILT = []


def build(repo, pr, checks, st):
    BUILT.append(f"{C.BUILD}built-{len(BUILT)}")
    return BUILT[-1]


P.build = build
kept_outputs = P.outputs
P.outputs = lambda repo, pr, checks, st: ["/nix/store/" + "x" * 32 + "-vm"] if checks == ["vm"] else None
fresh_pr_state()
STARTED.clear()
S.CFG["repos"]["sam/r"] = {"proofBuilds": {"vm": "checks.x86_64-linux.vm"}, "proofRuns": {"gate": "true"},
                           "proofJobs": {"other": "Run the other proof."}}


def pending(*names, passed=()):
    return CI_GREEN + [{"__typename": "StatusContext", "context": c, "state": "SUCCESS" if c in passed else "PENDING",
                        "description": ""} for c in names]


status, _ = run(clean(sha=HEAD, checks=pending("vm", "gate", "other")), 700)
assert BUILT == [f"{C.BUILD}built-0"] and STARTED == [] and JOBS == [] and "building vm here" in status, (status, BUILT, STARTED)
STATES[BUILT[0]] = "done"
status, _ = run(clean(sha=HEAD, checks=pending("vm", "gate", "other", passed=["vm"])), 900)
assert len(STARTED) == 1 and JOBS == [] and "Settled; running gate here" in status and "again" not in status, (status, JOBS)
finish(STARTED[0], verdicts={"gate": PASSED})
status, _ = run(clean(sha=HEAD, checks=pending("vm", "gate", "other", passed=["vm", "gate"])), 1100)
assert len(JOBS) == 1 and "Run the other proof." in JOBS[0] and "again" not in status, (status, JOBS)
assert "provedAgain" not in pr_state(), pr_state()
P.build, P.outputs = kept_build, kept_outputs
# A verdict left unposted belongs to its check's kind and command: one
# remapped, dropped or made the other kind since (a build's attribute and
# a run's command spelled the same) gets none of it.
KINDS = {"proofBuilds": {}, "proofRuns": {"gate": "make gate"}}
WAS = ("success", "x")
assert P.still({"provedFor": ["run", ["gate"], {"gate": "make gate"}]}, {"gate": WAS}, KINDS) == {"gate": WAS}
assert P.still({"provedFor": ["run", ["gate"], {"gate": "make gate"}]}, {"gate": WAS},
               {"proofBuilds": {}, "proofRuns": {"gate": "make gate --fast"}}) == {}
assert P.still({"provedFor": ["run", ["gate"], {"gate": "make gate"}]}, {"gate": WAS},
               {"proofBuilds": {"gate": "make gate"}, "proofRuns": {}}) == {}
assert P.still({"provedFor": ["build", ["gate"], {"gate": "make gate"}]}, {"gate": WAS}, KINDS) == {}
# A run's command remapped while it's pending is run again: the old one
# says nothing of the new.
fresh_pr_state()
STARTED.clear()
S.CFG["repos"]["sam/r"] = {"proofRuns": {"gate": "true"}}
run(clean(sha=HEAD, checks=GATE), 700)
finish(STARTED[0], verdicts={"gate": PASSED})
S.CFG["repos"]["sam/r"] = {"proofRuns": {"gate": "true --again"}}
status, _ = run(clean(sha=HEAD, checks=GATE), 900)
assert len(STARTED) == 2 and statuses() == [] and "running gate here" in status, (status, STARTED, WROTE)
# What a fix job is told of a run: the command, where it runs, and that the
# steward posts its status. A check in proofBuilds is the build, one in
# proofRuns and proofJobs the run, however each spells it.
rc = {"proofJobs": {"GATE": "Run it.", "x": "Run x."}, "proofBuilds": {"vm": "checks.x86_64-linux.vm"},
      "proofRuns": {"gate": "make gate 'quoted'", "VM": "not this"}}
told = C.machine(rc)
assert told.keys() == {"vm", "gate", "x"}, told
assert "bash -c 'make gate '\"'\"'quoted'\"'\"''" in told["gate"] and "never post that status yourself" in told["gate"], told
assert "only the commit fetched" in told["gate"] and "PROOF_BASE" not in told["gate"], told
assert told["vm"].startswith("`nix build .#checks.x86_64-linux.vm -L`") and told["x"] == "Run x.", told
print("proof runs' verdicts: ok")

P.start_run, P.build, C.gh = kept_start, kept_build, kept_gh
del S.CFG["repos"]["sam/r"]
S.CFG["projectsDir"] = kept_projects
os.environ["PATH"] = kept_path
STATES.clear()
